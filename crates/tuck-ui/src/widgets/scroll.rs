use std::time::Duration;

use tuck_core::{PointF, RectF};

use super::{Interaction, Response};
use crate::anim::{Animated, Motion, Spring};
use crate::color::Color;
use crate::event::Event;
use crate::painter::Painter;
use crate::view::Ctx;

const BAR_WIDTH: f32 = 5.0;
const BAR_WIDTH_HOVER: f32 = 7.0;
const BAR_INSET: f32 = 3.0;
const BAR_MARGIN: f32 = 4.0;
const MIN_THUMB: f32 = 28.0;
const HIDE_DELAY: Duration = Duration::from_millis(900);
const SMOOTHING: Spring = Spring { stiffness: 520.0, damping_ratio: 1.0 };

/// Vertical scroll container: spring-smoothed wheel scrolling (touchpads scroll 1:1), an overlay scrollbar that
/// fades in while scrolling and out 900 ms later, and a draggable thumb. Content coordinates are relative to the
/// content's top-left; `paint_content` clips and translates for you.
#[derive(Clone, Debug)]
pub struct ScrollView {
    viewport: RectF,
    content_height: f32,
    offset: Animated<f32>,
    target: f32,
    bar: Animated<f32>,
    timer_token: u64,
    thumb: Interaction,
    drag: Option<(f32, f32)>,
}

impl ScrollView {
    /// DIP scrolled per wheel notch.
    pub const WHEEL_STEP: f32 = 64.0;

    /// `timer_token` is the `Event::Timer` token the view reserves for hiding the scrollbar.
    pub fn new(timer_token: u64) -> Self {
        Self {
            viewport: RectF::default(),
            content_height: 0.0,
            offset: Animated::with_motion(0.0, Motion::Spring(SMOOTHING)),
            target: 0.0,
            bar: Animated::fade(0.0),
            timer_token,
            thumb: Interaction::new(),
            drag: None,
        }
    }

    pub fn viewport(&self) -> RectF {
        self.viewport
    }

    pub fn set_viewport(&mut self, viewport: RectF) {
        self.viewport = viewport;
        self.clamp_target();
    }

    pub fn content_height(&self) -> f32 {
        self.content_height
    }

    /// Updates the content height; an offset past the new end springs back.
    pub fn set_content_height(&mut self, height: f32) {
        self.content_height = height.max(0.0);
        self.clamp_target();
    }

    fn clamp_target(&mut self) {
        let clamped = self.target.clamp(0.0, self.max_offset());
        if clamped != self.target {
            self.target = clamped;
            self.offset.set(clamped);
        }
    }

    pub fn max_offset(&self) -> f32 {
        (self.content_height - self.viewport.h).max(0.0)
    }

    /// Current (animated) offset of the content top above the viewport top.
    pub fn offset(&self) -> f32 {
        self.offset.get()
    }

    /// Where the offset is heading.
    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn can_scroll(&self) -> bool {
        self.max_offset() > 0.5
    }

    pub fn scroll_to(&mut self, cx: &mut Ctx, target: f32, animate: bool) {
        let target = target.clamp(0.0, self.max_offset());
        self.target = target;
        if animate {
            self.offset.set(target);
        } else {
            self.offset.snap(target);
        }
        self.flash_scrollbar(cx);
        cx.request_paint();
    }

    /// Jumps without showing the scrollbar (opening, resets, previews).
    pub fn snap_to(&mut self, target: f32) {
        self.target = target.clamp(0.0, self.max_offset());
        self.offset.snap(self.target);
    }

    /// Scrolls the least amount that shows content rows `top..bottom` with `margin` around them.
    pub fn reveal(&mut self, cx: &mut Ctx, top: f32, bottom: f32, margin: f32) {
        let view_h = self.viewport.h;
        let target = if top - margin < self.target {
            top - margin
        } else if bottom + margin > self.target + view_h {
            bottom + margin - view_h
        } else {
            return;
        };
        self.scroll_to(cx, target, true);
    }

    /// Window position of the content origin right now.
    pub fn content_origin(&self) -> PointF {
        PointF::new(self.viewport.x, self.viewport.y - self.offset())
    }

    /// Window → content coordinates.
    pub fn to_content(&self, pos: PointF) -> PointF {
        let origin = self.content_origin();
        PointF::new(pos.x - origin.x, pos.y - origin.y)
    }

    /// Content → window coordinates.
    pub fn to_window(&self, rect: RectF) -> RectF {
        let origin = self.content_origin();
        rect.offset(origin.x, origin.y)
    }

    /// The content rows currently inside the viewport.
    pub fn visible_range(&self) -> (f32, f32) {
        let top = self.offset();
        (top, top + self.viewport.h)
    }

    /// Shows the scrollbar and schedules its fade-out.
    pub fn flash_scrollbar(&mut self, cx: &mut Ctx) {
        if !self.can_scroll() {
            return;
        }
        self.bar.set(1.0);
        cx.set_timer(HIDE_DELAY, self.timer_token);
    }

    /// Previews: scrollbar opacity without animation.
    pub fn force_scrollbar(&mut self, opacity: f32) {
        self.bar.snap(opacity);
    }

    fn track(&self) -> RectF {
        let x = self.viewport.right() - BAR_INSET - BAR_WIDTH_HOVER;
        RectF::new(x, self.viewport.y + BAR_MARGIN, BAR_WIDTH_HOVER, (self.viewport.h - 2.0 * BAR_MARGIN).max(0.0))
    }

    fn thumb_rect(&self) -> RectF {
        let track = self.track();
        let thumb_h = (track.h * self.viewport.h / self.content_height.max(1.0)).clamp(MIN_THUMB.min(track.h), track.h);
        let progress = if self.can_scroll() { (self.offset() / self.max_offset()).clamp(0.0, 1.0) } else { 0.0 };
        RectF::new(track.x, track.y + (track.h - thumb_h) * progress, track.w, thumb_h)
    }

    pub fn event(&mut self, cx: &mut Ctx, event: &Event) -> Response<()> {
        match event {
            Event::Timer(token) if *token == self.timer_token => {
                if self.drag.is_none() && !self.thumb.hovered {
                    self.bar.set(0.0);
                    cx.request_paint();
                } else {
                    cx.set_timer(HIDE_DELAY, self.timer_token);
                }
                Response::Consumed
            }
            Event::Wheel(wheel) if self.viewport.contains(wheel.pos) && self.can_scroll() && wheel.delta.y != 0.0 => {
                let target = self.target - wheel.delta.y * Self::WHEEL_STEP;
                self.scroll_to(cx, target, !wheel.precise);
                Response::Consumed
            }
            _ => self.thumb_event(cx, event),
        }
    }

    fn thumb_event(&mut self, cx: &mut Ctx, event: &Event) -> Response<()> {
        if !self.can_scroll() {
            return Response::Ignored;
        }
        let thumb = self.thumb_rect().inset(-2.0);
        let was_hovered = self.thumb.hovered;
        let visible = self.bar.target() > 0.0;
        self.thumb.update(event, thumb, visible || self.drag.is_some());
        if was_hovered != self.thumb.hovered {
            if self.thumb.hovered {
                self.bar.set(1.0);
            } else {
                cx.set_timer(HIDE_DELAY, self.timer_token);
            }
            cx.request_paint();
        }
        match event {
            Event::PointerDown(e) if self.thumb.pressed => {
                self.drag = Some((e.pos.y, self.offset()));
                Response::Consumed
            }
            Event::PointerMove(e) => match self.drag {
                Some((start_y, start_offset)) => {
                    let track = self.track();
                    let travel = (track.h - self.thumb_rect().h).max(1.0);
                    let target = start_offset + (e.pos.y - start_y) * self.max_offset() / travel;
                    self.scroll_to(cx, target, false);
                    Response::Consumed
                }
                None => Response::Ignored,
            },
            Event::PointerUp(_) | Event::PointerCancel if self.drag.is_some() => {
                self.drag = None;
                cx.set_timer(HIDE_DELAY, self.timer_token);
                Response::Consumed
            }
            _ => Response::Ignored,
        }
    }

    /// Paints `f` in content coordinates, clipped to the viewport.
    pub fn paint_content<R>(&self, p: &mut Painter, f: impl FnOnce(&mut Painter) -> R) -> R {
        let origin = self.content_origin();
        let dy = p.snap(origin.y) - origin.y;
        p.clip_rect(self.viewport, |p| p.translate(origin.x, origin.y + dy, f))
    }

    pub fn paint_scrollbar(&self, p: &mut Painter) {
        let opacity = self.bar.get().clamp(0.0, 1.0);
        if opacity <= 0.0 || !self.can_scroll() {
            return;
        }
        let theme = p.theme().clone();
        let widen = if self.thumb.hovered || self.drag.is_some() { 1.0 } else { 0.0 };
        let width = BAR_WIDTH + (BAR_WIDTH_HOVER - BAR_WIDTH) * widen;
        let thumb = self.thumb_rect();
        let rect = RectF::new(thumb.right() - width, thumb.y, width, thumb.h);
        let ink = if theme.is_dark() { Color::rgba(1.0, 1.0, 1.0, 0.38) } else { Color::rgba(0.0, 0.0, 0.0, 0.32) };
        p.fill_round_rect(p.snap_rect(rect), width / 2.0, ink.multiply_alpha(opacity));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_clamp_to_the_content() {
        let mut scroll = ScrollView::new(1);
        scroll.set_viewport(RectF::new(0.0, 100.0, 300.0, 200.0));
        scroll.set_content_height(1000.0);
        assert_eq!(scroll.max_offset(), 800.0);
        scroll.snap_to(5000.0);
        assert_eq!(scroll.target(), 800.0);
        scroll.set_content_height(500.0);
        assert_eq!(scroll.target(), 300.0, "shrinking content pulls the offset back");
        scroll.snap_to(300.0);
        assert_eq!(scroll.to_content(PointF::new(10.0, 100.0)), PointF::new(10.0, 300.0));
        assert_eq!(scroll.visible_range(), (300.0, 500.0));
    }
}
