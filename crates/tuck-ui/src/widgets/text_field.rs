use std::ops::Range;
use std::time::Duration;

use tuck_core::{PointF, RectF};

use super::{Interaction, Response};
use crate::anim::{Animated, now};
use crate::color::Color;
use crate::cursor::Cursor;
use crate::event::{Event, Key, KeyEvent, MouseButton};
use crate::gfx::Gfx;
use crate::icons::Icon;
use crate::painter::Painter;
use crate::text::TextStyle;
use crate::view::Ctx;

const RADIUS: f32 = 8.0;
const PADDING: f32 = 10.0;
const ICON: f32 = 16.0;
const GAP: f32 = 6.0;
const CLEAR: f32 = 16.0;
const CARET_WIDTH: f32 = 1.5;

/// Single-line text field (search boxes): leading icon, placeholder, blinking accent caret, select-all and a clear
/// button. It never needs real keyboard focus: feed it `KeyDown`/`Text` events from wherever keys come from.
/// `Action(())` = the text changed.
#[derive(Clone, Debug)]
pub struct TextField {
    text: String,
    /// Byte offsets on cluster boundaries; `anchor != caret` is a selection.
    caret: usize,
    anchor: usize,
    placeholder: String,
    icon: Option<Icon>,
    style: TextStyle,
    rect: RectF,
    text_area: RectF,
    scroll_x: f32,
    focused: bool,
    edited_at: f64,
    forced_caret: Option<bool>,
    clear: Interaction,
    clear_shown: Animated<f32>,
    selecting: bool,
    over_text: bool,
}

impl TextField {
    /// Caret on/off half period (DESIGN §9).
    pub const BLINK: Duration = Duration::from_millis(530);

    pub fn new(placeholder: &str) -> Self {
        Self {
            text: String::new(),
            caret: 0,
            anchor: 0,
            placeholder: placeholder.to_string(),
            icon: None,
            style: TextStyle::body(),
            rect: RectF::default(),
            text_area: RectF::default(),
            scroll_x: 0.0,
            focused: true,
            edited_at: f64::NEG_INFINITY,
            forced_caret: None,
            clear: Interaction::new(),
            clear_shown: Animated::fade(0.0),
            selecting: false,
            over_text: false,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn style(mut self, style: TextStyle) -> Self {
        self.style = style;
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Replaces the text; the caret goes to the end.
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.caret = self.text.len();
        self.anchor = self.caret;
        self.scroll_x = 0.0;
        self.sync_clear();
    }

    pub fn set_placeholder(&mut self, placeholder: &str) {
        self.placeholder = placeholder.to_string();
    }

    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }

    pub fn rect(&self) -> RectF {
        self.rect
    }

    pub fn set_rect(&mut self, rect: RectF) {
        self.rect = rect;
    }

    /// The caret as a byte offset into `text()`.
    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn caret_at_end(&self) -> bool {
        self.caret == self.text.len() && !self.has_selection()
    }

    pub fn selection(&self) -> Option<Range<usize>> {
        self.has_selection().then(|| self.caret.min(self.anchor)..self.caret.max(self.anchor))
    }

    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }

    /// A focused field shows its blinking caret.
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.restart_blink();
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// Shows the caret solid for one blink period (call when the field appears or after edits).
    pub fn restart_blink(&mut self) {
        self.edited_at = now();
    }

    /// Previews: caret always shown (`Some(true)`), hidden, or blinking (`None`).
    pub fn force_caret(&mut self, visible: Option<bool>) {
        self.forced_caret = visible;
    }

    pub fn force_clear_hover(&mut self, hovered: bool) {
        self.clear.force(hovered, false);
    }

    /// Time until the caret next appears or disappears.
    pub fn next_blink(&self) -> Duration {
        let period = Self::BLINK.as_secs_f64();
        let elapsed = (now() - self.edited_at).max(0.0);
        Duration::from_secs_f64((period - elapsed % period).max(0.001))
    }

    fn caret_visible(&self) -> bool {
        if let Some(forced) = self.forced_caret {
            return forced;
        }
        if !self.focused {
            return false;
        }
        let elapsed = now() - self.edited_at;
        elapsed < Self::BLINK.as_secs_f64() || (elapsed / Self::BLINK.as_secs_f64()) as i64 % 2 == 0
    }

    fn edited(&mut self) {
        self.edited_at = now();
        self.sync_clear();
    }

    fn sync_clear(&mut self) {
        self.clear_shown.set(if self.text.is_empty() { 0.0 } else { 1.0 });
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
        self.edited_at = now();
    }

    /// Clears the text; returns true if there was any.
    pub fn clear(&mut self) -> bool {
        if self.text.is_empty() {
            return false;
        }
        self.set_text("");
        self.edited();
        true
    }

    /// Inserts `text` at the caret, replacing the selection. Control characters are dropped.
    pub fn insert(&mut self, text: &str) -> bool {
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        if clean.is_empty() {
            return false;
        }
        self.delete_selection();
        self.text.insert_str(self.caret, &clean);
        self.caret += clean.len();
        self.anchor = self.caret;
        self.edited();
        true
    }

    fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else { return false };
        self.text.replace_range(range.clone(), "");
        self.caret = range.start;
        self.anchor = range.start;
        true
    }

    /// Backspace: the selection, else one cluster (or one word) before the caret.
    pub fn delete_backward(&mut self, gfx: &Gfx, word: bool) -> bool {
        if self.delete_selection() {
            self.edited();
            return true;
        }
        if self.caret == 0 {
            return false;
        }
        let start = if word { self.word_start_before(gfx, self.caret) } else { self.previous_boundary(gfx, self.caret) };
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
        self.anchor = start;
        self.edited();
        true
    }

    /// Delete: the selection, else one cluster (or one word) after the caret.
    pub fn delete_forward(&mut self, gfx: &Gfx, word: bool) -> bool {
        if self.delete_selection() {
            self.edited();
            return true;
        }
        if self.caret >= self.text.len() {
            return false;
        }
        let end = if word { self.word_end_after(gfx, self.caret) } else { self.next_boundary(gfx, self.caret) };
        self.text.replace_range(self.caret..end, "");
        self.anchor = self.caret;
        self.edited();
        true
    }

    /// Moves the caret one cluster (or word) left or right; `extend` grows the selection. Returns true if it moved
    /// or a selection collapsed.
    pub fn move_caret(&mut self, gfx: &Gfx, forward: bool, word: bool, extend: bool) -> bool {
        let before = (self.caret, self.anchor);
        let target = match (self.selection(), extend, forward) {
            (Some(range), false, false) => range.start,
            (Some(range), false, true) => range.end,
            (_, _, false) if word => self.word_start_before(gfx, self.caret),
            (_, _, false) => self.previous_boundary(gfx, self.caret),
            (_, _, true) if word => self.word_end_after(gfx, self.caret),
            (_, _, true) => self.next_boundary(gfx, self.caret),
        };
        self.caret = target;
        if !extend {
            self.anchor = target;
        }
        self.edited_at = now();
        before != (self.caret, self.anchor)
    }

    pub fn move_to_edge(&mut self, end: bool, extend: bool) -> bool {
        let before = (self.caret, self.anchor);
        self.caret = if end { self.text.len() } else { 0 };
        if !extend {
            self.anchor = self.caret;
        }
        self.edited_at = now();
        before != (self.caret, self.anchor)
    }

    /// Cluster boundaries as byte offsets (emoji sequences and combining marks stay whole).
    fn boundaries(&self, gfx: &Gfx) -> Vec<usize> {
        let char_starts = || self.text.char_indices().map(|(i, _)| i).chain([self.text.len()]).collect::<Vec<_>>();
        let Ok(layout) = gfx.text_layout(&self.text, &self.style, None) else { return char_starts() };
        let mut utf16_to_byte = Vec::with_capacity(self.text.len() + 1);
        let mut utf16 = 0u32;
        for (byte, c) in self.text.char_indices() {
            utf16_to_byte.push((utf16, byte));
            utf16 += c.len_utf16() as u32;
        }
        utf16_to_byte.push((utf16, self.text.len()));
        let boundaries: Vec<usize> = layout
            .cluster_boundaries()
            .into_iter()
            .filter_map(|u| utf16_to_byte.binary_search_by_key(&u, |(u16_offset, _)| *u16_offset).ok().map(|i| utf16_to_byte[i].1))
            .collect();
        if boundaries.last() == Some(&self.text.len()) { boundaries } else { char_starts() }
    }

    fn previous_boundary(&self, gfx: &Gfx, from: usize) -> usize {
        self.boundaries(gfx).into_iter().rev().find(|&b| b < from).unwrap_or(0)
    }

    fn next_boundary(&self, gfx: &Gfx, from: usize) -> usize {
        self.boundaries(gfx).into_iter().find(|&b| b > from).unwrap_or(self.text.len())
    }

    fn word_start_before(&self, gfx: &Gfx, from: usize) -> usize {
        let head = &self.text[..from];
        let trimmed = head.trim_end();
        let start = trimmed.rfind(char::is_whitespace).map_or(0, |i| i + trimmed[i..].chars().next().map_or(1, char::len_utf8));
        self.boundaries(gfx).into_iter().rev().find(|&b| b <= start).unwrap_or(0)
    }

    fn word_end_after(&self, gfx: &Gfx, from: usize) -> usize {
        let tail = &self.text[from..];
        let skipped = tail.len() - tail.trim_start().len();
        let word = tail[skipped..].find(char::is_whitespace).unwrap_or(tail.len() - skipped);
        let end = from + skipped + word;
        self.boundaries(gfx).into_iter().find(|&b| b >= end).unwrap_or(self.text.len())
    }

    /// Byte offset of the caret stop nearest to `x` (window DIP).
    fn offset_at(&self, gfx: &Gfx, x: f32) -> usize {
        let Ok(layout) = gfx.text_layout(&self.text, &self.style, None) else { return self.text.len() };
        let utf16 = layout.hit_test(PointF::new(x - self.text_area.x + self.scroll_x, layout.baseline * 0.5)) as usize;
        let mut units = 0;
        for (byte, c) in self.text.char_indices() {
            if units >= utf16 {
                return byte;
            }
            units += c.len_utf16();
        }
        self.text.len()
    }

    fn utf16_index(&self, byte: usize) -> u32 {
        self.text[..byte.min(self.text.len())].encode_utf16().count() as u32
    }

    pub fn clear_button_rect(&self) -> RectF {
        let size = CLEAR + 8.0;
        RectF::new(self.rect.right() - PADDING + 4.0 - size, self.rect.center().y - size / 2.0, size, size)
    }

    /// Text editing keys: Backspace/Delete (Ctrl = word), Ctrl+A, Left/Right (Ctrl = word, Shift = select),
    /// Home/End. Anything else is `Ignored` so the owner can use it.
    pub fn key(&mut self, gfx: &Gfx, key: &KeyEvent) -> Response<()> {
        let (ctrl, shift) = (key.mods.ctrl, key.mods.shift);
        let changed = match key.key {
            Key::Backspace => self.delete_backward(gfx, ctrl),
            Key::Delete => self.delete_forward(gfx, ctrl),
            Key::Char('A') if ctrl && !shift && !key.mods.alt => {
                self.select_all();
                false
            }
            Key::Left | Key::Right => {
                self.move_caret(gfx, key.key == Key::Right, ctrl, shift);
                false
            }
            Key::Home | Key::End => {
                self.move_to_edge(key.key == Key::End, shift);
                false
            }
            _ => return Response::Ignored,
        };
        if changed { Response::Action(()) } else { Response::Consumed }
    }

    pub fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<()> {
        let clear_enabled = !self.text.is_empty();
        let clear_rect = self.clear_button_rect();
        let was_hovered = self.clear.hovered;
        if let Response::Action(()) = self.clear.update(event, clear_rect, clear_enabled) {
            self.clear();
            cx.request_paint();
            return Response::Action(());
        }
        if was_hovered != self.clear.hovered {
            cx.request_paint();
        }
        if self.clear.pressed {
            return Response::Consumed;
        }
        match event {
            Event::Text(text) => {
                if self.insert(text) {
                    cx.request_paint();
                    Response::Action(())
                } else {
                    Response::Ignored
                }
            }
            Event::KeyDown(key) => {
                let response = self.key(cx.gfx(), key);
                if response.consumed() {
                    cx.request_paint();
                }
                response
            }
            Event::PointerDown(e) if e.button == Some(MouseButton::Left) && self.rect.contains(e.pos) => {
                if e.click_count >= 2 {
                    self.select_all();
                } else {
                    let offset = self.offset_at(cx.gfx(), e.pos.x);
                    self.caret = offset;
                    if !e.mods.shift {
                        self.anchor = offset;
                    }
                    self.selecting = true;
                }
                self.edited_at = now();
                cx.request_paint();
                Response::Consumed
            }
            Event::PointerMove(e) => {
                let over = self.text_area.contains(e.pos) || self.selecting;
                if over != self.over_text {
                    self.over_text = over;
                    cx.set_cursor(if over { Cursor::IBeam } else { Cursor::Arrow });
                }
                if self.selecting {
                    self.caret = self.offset_at(cx.gfx(), e.pos.x);
                    cx.request_paint();
                    return Response::Consumed;
                }
                Response::Ignored
            }
            Event::PointerUp(_) | Event::PointerCancel if self.selecting => {
                self.selecting = false;
                Response::Consumed
            }
            Event::PointerLeave if self.over_text => {
                self.over_text = false;
                Response::Ignored
            }
            _ => Response::Ignored,
        }
    }

    pub fn paint(&mut self, p: &mut Painter) {
        let theme = p.theme().clone();
        let rect = p.snap_rect(self.rect);
        let dark = theme.is_dark();
        let fill = if dark { Color::rgba(1.0, 1.0, 1.0, 0.07) } else { Color::rgba(0.0, 0.0, 0.0, 0.05) };
        p.fill_round_rect(rect, RADIUS, fill);
        let mut left = rect.x + PADDING;
        if let Some(icon) = self.icon {
            p.icon(icon, PointF::new(left + ICON / 2.0, rect.center().y), ICON, theme.text_secondary);
            left += ICON + GAP;
        }
        let clear_amount = self.clear_shown.get().clamp(0.0, 1.0);
        let right = rect.right() - PADDING - if self.text.is_empty() { 0.0 } else { CLEAR + GAP };
        self.text_area = RectF::from_ltrb(left, rect.y, right.max(left), rect.bottom());
        let area = self.text_area;
        let style = self.style;
        if self.text.is_empty() {
            p.text(&self.placeholder, &style, theme.text_tertiary, area);
        }
        let layout = p.layout(&self.text, &style, None);
        let metrics = layout.as_ref().map(|l| l.metrics).unwrap_or_default();
        let baseline = p.snap(area.y + area.h * 0.5 + metrics.cap_height * 0.5);
        let caret_x = layout.as_ref().map_or(0.0, |l| l.caret_rect(self.utf16_index(self.caret)).x);
        let text_width = layout.as_ref().map_or(0.0, |l| l.width);
        let room = (area.w - CARET_WIDTH).max(0.0);
        if caret_x - self.scroll_x > room {
            self.scroll_x = caret_x - room;
        } else if caret_x < self.scroll_x {
            self.scroll_x = caret_x;
        }
        self.scroll_x = self.scroll_x.clamp(0.0, (text_width - room).max(0.0));
        let origin_x = area.x - self.scroll_x;
        let caret_top = baseline - metrics.cap_height - 3.0;
        let caret_height = metrics.cap_height + 6.0;
        p.clip_rect(RectF::new(area.x - CARET_WIDTH, rect.y, area.w + 2.0 * CARET_WIDTH, rect.h), |p| {
            if let (Some(layout), Some(range)) = (&layout, self.selection()) {
                let start = layout.caret_rect(self.utf16_index(range.start)).x;
                let end = layout.caret_rect(self.utf16_index(range.end)).x;
                let highlight = theme.accent.with_alpha(if dark { 0.42 } else { 0.28 });
                p.fill_rect(RectF::new(origin_x + start, caret_top, end - start, caret_height), highlight);
            }
            if let Some(layout) = &layout {
                p.draw_layout(layout, PointF::new(origin_x, baseline - layout.baseline), theme.text);
            }
            if self.caret_visible() && !self.has_selection() {
                let x = p.snap(origin_x + caret_x - CARET_WIDTH / 2.0);
                p.fill_rect(RectF::new(x, caret_top, CARET_WIDTH, caret_height), theme.accent);
            }
        });
        if clear_amount > 0.0 {
            let center = p.snap_point(self.clear_button_rect().center());
            let hover = self.clear.hover_amount();
            let disc = theme.text_tertiary.lerp(&theme.text_secondary, hover).multiply_alpha(clear_amount);
            p.fill_circle(center, CLEAR / 2.0, disc);
            let glyph = if dark { Color::rgba(0.0, 0.0, 0.0, 0.75) } else { Color::WHITE };
            p.icon_with_stroke(Icon::X, center, 9.0, glyph.multiply_alpha(clear_amount), 3.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gfx() -> std::rc::Rc<Gfx> {
        Gfx::new().expect("graphics devices")
    }

    #[test]
    fn typing_replaces_the_selection_and_backspace_keeps_clusters_whole() {
        let gfx = gfx();
        let mut field = TextField::new("Search");
        assert!(field.insert("thumbs"));
        field.select_all();
        assert!(field.insert("hi 👍🏽"));
        assert_eq!(field.text(), "hi 👍🏽");
        assert!(field.delete_backward(&gfx, false));
        assert_eq!(field.text(), "hi ", "the toned emoji is one cluster");
        assert!(field.delete_backward(&gfx, true));
        assert_eq!(field.text(), "");
        assert!(!field.delete_backward(&gfx, false));
    }

    #[test]
    fn caret_moves_by_cluster_and_word() {
        let gfx = gfx();
        let mut field = TextField::new("");
        field.set_text("red e\u{301}x blue");
        field.move_to_edge(false, false);
        field.move_caret(&gfx, true, true, false);
        assert_eq!(field.caret(), 3);
        field.move_caret(&gfx, true, false, false);
        field.move_caret(&gfx, true, false, false);
        assert_eq!(&field.text()[field.caret()..], "x blue", "e + combining acute is one stop");
        field.move_caret(&gfx, true, false, true);
        assert_eq!(field.selection(), Some(7..8));
        assert!(field.delete_forward(&gfx, false));
        assert_eq!(field.text(), "red e\u{301} blue");
        field.move_to_edge(true, false);
        assert!(field.caret_at_end());
    }

    #[test]
    fn control_characters_are_not_text() {
        let mut field = TextField::new("");
        assert!(!field.insert("\u{8}\r"));
        assert!(field.is_empty());
        assert!(field.insert("a\tb"));
        assert_eq!(field.text(), "ab");
    }
}
