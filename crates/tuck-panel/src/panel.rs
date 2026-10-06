//! The panel view: search row, tab bar, the four tabs, motion and the keyboard/mouse model of DESIGN §7.

use std::rc::Rc;

use tuck_core::{ClipId, PointI, SkinTone};
use tuck_ui::widgets::{IconButton, Presence, Segment, Segmented, SegmentedStyle, TextField};
use tuck_ui::{
    Animated, Bitmap, Ctx, CubicBezier, Event, Gfx, Icon, Key, KeyEvent, Motion, MouseButton, Painter, PanelBackdrop,
    PointF, RectF, SizeF, Tween, View,
};

use crate::clipboard::ClipboardTab;
use crate::format::Clock;
use crate::picker::{self, PickerKind, PickerTab};
use crate::style::{
    self, BLINK_TIMER, BOTTOM_BAR, CONTENT_TOP, GEAR, LABEL_TIMER, LONG_PRESS_TIMER, PADDING, PANEL_SIZE,
    PICKER_SCROLL_TIMERS, SEARCH_HEIGHT, SEARCH_TOP, TABS_HEIGHT, TABS_TOP, WINDOW_RADIUS,
};
use crate::{ClipRow, PanelAction, PanelOptions, PickerSection, Tab};

const TAB_SLIDE: f32 = 12.0;
/// Emoji queued for background rasterization when sections arrive (about four screens).
const EMOJI_PREWARM: usize = 400;
const TAB_SWITCH: Tween = Tween { duration: 0.22, curve: CubicBezier::STANDARD };

/// What the tabs need to know about the panel while handling an event or painting.
#[derive(Clone, Debug)]
pub(crate) struct Env {
    pub options: PanelOptions,
    pub query: String,
    pub ctrl_held: bool,
    pub clock: Clock,
    pub size: SizeF,
}

struct TabSwitch {
    from: Tab,
    progress: Animated<f32>,
    direction: f32,
}

/// The panel (DESIGN §7): a `tuck_ui::View` for the pre-created, never-activated panel window. Data comes in
/// through the setters; results go out as `PanelAction`s posted with `cx.post`.
pub struct Panel {
    options: PanelOptions,
    backdrop: PanelBackdrop,
    tab: Tab,
    search: TextField,
    gear: IconButton,
    tabs: Segmented,
    clipboard: ClipboardTab,
    pickers: [PickerTab; 3],
    size: SizeF,
    presence: Presence,
    closing: bool,
    switch: Option<TabSwitch>,
    ctrl_held: bool,
    drag: Option<(PointI, PointI)>,
    clock: Option<Clock>,
    utc_offset_minutes: i32,
    outbox: Vec<PanelAction>,
    #[cfg(test)]
    pub(crate) posted: Vec<PanelAction>,
}

impl Panel {
    /// The window size the layout is designed for, DIP.
    pub const SIZE: SizeF = PANEL_SIZE;

    pub fn new(options: PanelOptions) -> Self {
        let tabs = Segmented::new(
            vec![
                Segment::icon_label(Tab::Clipboard.icon(), Tab::Clipboard.title()).tooltip("Clipboard history", Some("Win+V")),
                Segment::icon_label(Tab::Emoji.icon(), Tab::Emoji.title()).tooltip("Emoji", Some("Win+.")),
                Segment::icon_label(Tab::Kaomoji.icon(), Tab::Kaomoji.title()).tooltip("Kaomoji", None),
                Segment::icon_label(Tab::Symbols.icon(), Tab::Symbols.title()).tooltip("Symbols", None),
            ],
            0,
        )
        .style(SegmentedStyle::Track)
        .compact();
        Self {
            options,
            backdrop: PanelBackdrop::default(),
            tab: Tab::Clipboard,
            search: TextField::new(Tab::Clipboard.placeholder()).icon(Icon::Search),
            gear: IconButton::new(Icon::Settings).tooltip("Settings", None),
            tabs,
            clipboard: ClipboardTab::new(),
            pickers: [
                PickerTab::new(PickerKind::Emoji, PICKER_SCROLL_TIMERS[0]),
                PickerTab::new(PickerKind::Kaomoji, PICKER_SCROLL_TIMERS[1]),
                PickerTab::new(PickerKind::Symbols, PICKER_SCROLL_TIMERS[2]),
            ],
            size: PANEL_SIZE,
            presence: Presence::new(false),
            closing: false,
            switch: None,
            ctrl_held: false,
            drag: None,
            clock: None,
            utc_offset_minutes: 0,
            outbox: Vec::new(),
            #[cfg(test)]
            posted: Vec::new(),
        }
    }

    pub fn set_options(&mut self, cx: &mut Ctx, options: PanelOptions) {
        if options.paused != self.options.paused {
            self.clipboard.mark_dirty();
        }
        self.options = options;
        cx.request_paint();
    }

    pub fn options(&self) -> PanelOptions {
        self.options
    }

    pub fn skin_tone(&self) -> SkinTone {
        self.options.skin_tone
    }

    /// Which material the window has (Solid = paint an opaque fill). Default `SystemAcrylic`.
    pub fn set_backdrop(&mut self, backdrop: PanelBackdrop) {
        self.backdrop = backdrop;
    }

    /// Fixes "now" for relative times (previews); None = the system clock.
    pub fn set_clock(&mut self, clock: Option<Clock>) {
        self.clock = clock;
    }

    /// Shows the panel on `tab`: clears the query and selection, scrolls to the top and plays the appear motion.
    pub fn open(&mut self, cx: &mut Ctx, tab: Tab) {
        self.tab = tab;
        self.tabs.set_selected(tab.index());
        self.tabs.layout(cx.gfx(), self.tabs_rect());
        self.search.set_text("");
        self.search.set_placeholder(tab.placeholder());
        self.search.set_focused(true);
        self.clipboard.reset();
        for picker in &mut self.pickers {
            picker.reset();
        }
        self.switch = None;
        self.closing = false;
        self.ctrl_held = false;
        self.drag = None;
        self.utc_offset_minutes = tuck_ui::win::utc_offset_minutes();
        self.presence.snap(false);
        self.presence.set_shown(true);
        cx.set_timer(TextField::BLINK, BLINK_TIMER);
        cx.request_paint();
    }

    /// Starts the disappear motion; hide the window once `is_closing_done()`.
    pub fn close(&mut self, cx: &mut Ctx) {
        self.closing = true;
        self.presence.set_shown(false);
        self.search.set_focused(false);
        cx.hide_tooltip();
        cx.request_paint();
    }

    pub fn is_closing_done(&self) -> bool {
        self.closing && !self.presence.is_visible()
    }

    pub fn is_open(&self) -> bool {
        self.presence.is_shown()
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn query(&self) -> &str {
        self.search.text()
    }

    /// Switches the open panel to `tab` as clicking its segment would (the other hotkey while open).
    pub fn select_tab(&mut self, cx: &mut Ctx, tab: Tab) {
        self.switch_tab(cx, tab);
        self.flush(cx);
    }

    /// Clipboard rows, already filtered by the query; `total` is the size of the whole history.
    pub fn set_clips(&mut self, cx: &mut Ctx, rows: Vec<ClipRow>, total: usize) {
        let animate = self.presence.is_shown() && !self.closing;
        self.clipboard.set_rows(rows, total, animate);
        cx.request_paint();
    }

    pub fn set_thumbnail(&mut self, cx: &mut Ctx, id: ClipId, bitmap: Rc<Bitmap>) {
        if self.clipboard.set_thumbnail(id, bitmap) {
            cx.request_paint();
        }
    }

    /// Sections for a picker tab (ignored for Clipboard). While the query is non-empty they are search results.
    pub fn set_sections(&mut self, cx: &mut Ctx, tab: Tab, sections: Vec<PickerSection>) {
        let searching = !self.search.is_empty();
        let tone = self.options.skin_tone;
        if let Some(picker) = self.picker_mut(tab) {
            picker.set_sections(sections, searching, searching);
            if tab == Tab::Emoji {
                let px = (picker::EMOJI_SIZE * cx.scale()).round() as u32;
                cx.gfx().prewarm_emoji(&picker.display_texts(tone, EMOJI_PREWARM), px);
            }
            cx.request_paint();
        }
    }

    /// The visible panel inside the window, DIP (the window rect is the panel rect).
    pub fn panel_rect(&self) -> RectF {
        RectF::new(0.0, 0.0, self.size.w, self.size.h)
    }

    fn picker_mut(&mut self, tab: Tab) -> Option<&mut PickerTab> {
        match tab {
            Tab::Clipboard => None,
            Tab::Emoji => Some(&mut self.pickers[0]),
            Tab::Kaomoji => Some(&mut self.pickers[1]),
            Tab::Symbols => Some(&mut self.pickers[2]),
        }
    }

    fn env(&self) -> Env {
        let clock = self
            .clock
            .unwrap_or_else(|| Clock { now_ms: tuck_core::now_ms(), utc_offset_minutes: self.utc_offset_minutes });
        Env { options: self.options, query: self.search.text().to_string(), ctrl_held: self.ctrl_held, clock, size: self.size }
    }

    fn search_rect(&self) -> RectF {
        RectF::new(PADDING, SEARCH_TOP, self.size.w - 2.0 * PADDING - GEAR - 8.0, SEARCH_HEIGHT)
    }

    fn gear_rect(&self) -> RectF {
        RectF::new(self.size.w - PADDING - GEAR, SEARCH_TOP + (SEARCH_HEIGHT - GEAR) / 2.0, GEAR, GEAR)
    }

    fn tabs_rect(&self) -> RectF {
        RectF::new(PADDING, TABS_TOP, self.size.w - 2.0 * PADDING, TABS_HEIGHT)
    }

    fn viewport(&self) -> RectF {
        RectF::new(0.0, CONTENT_TOP, self.size.w, (self.size.h - CONTENT_TOP - BOTTOM_BAR).max(0.0))
    }

    fn bar_rect(&self) -> RectF {
        RectF::new(0.0, self.size.h - BOTTOM_BAR, self.size.w, BOTTOM_BAR)
    }

    fn layout(&mut self, gfx: &Gfx) {
        self.search.set_rect(self.search_rect());
        self.gear.set_rect(self.gear_rect());
        self.tabs.layout(gfx, self.tabs_rect());
        let (viewport, bar) = (self.viewport(), self.bar_rect());
        self.clipboard.set_frame(viewport, bar);
        for picker in &mut self.pickers {
            picker.set_frame(viewport, bar);
        }
    }

    fn flush(&mut self, cx: &mut Ctx) {
        for action in self.outbox.drain(..) {
            #[cfg(test)]
            self.posted.push(action.clone());
            cx.post(action);
        }
    }

    fn switch_tab(&mut self, cx: &mut Ctx, tab: Tab) {
        if tab == self.tab {
            return;
        }
        let direction = if tab.index() > self.tab.index() { 1.0 } else { -1.0 };
        let mut progress = Animated::with_motion(0.0, Motion::Tween(TAB_SWITCH));
        progress.set(1.0);
        self.switch = Some(TabSwitch { from: self.tab, progress, direction });
        self.tab = tab;
        self.tabs.set_selected(tab.index());
        self.search.set_placeholder(tab.placeholder());
        cx.hide_tooltip();
        self.outbox.push(PanelAction::TabChanged(tab));
        if !self.search.is_empty() {
            self.outbox.push(PanelAction::QueryChanged { tab, query: self.search.text().to_string() });
        }
        cx.request_paint();
    }

    fn query_changed(&mut self, cx: &mut Ctx) {
        self.outbox.push(PanelAction::QueryChanged { tab: self.tab, query: self.search.text().to_string() });
        self.search.restart_blink();
        cx.request_paint();
    }

    fn tab_has_overlay(&self) -> bool {
        match self.tab {
            Tab::Clipboard => self.clipboard.has_overlay(),
            tab => self.pickers[tab.index() - 1].has_overlay(),
        }
    }

    fn dismiss_overlay(&mut self) -> bool {
        match self.tab {
            Tab::Clipboard => self.clipboard.dismiss_overlay(),
            tab => self.pickers[tab.index() - 1].dismiss_overlay(),
        }
    }

    fn tab_key(&mut self, cx: &mut Ctx, key: &KeyEvent) -> bool {
        let env = self.env();
        match self.tab {
            Tab::Clipboard => self.clipboard.key(cx, key, &env, &mut self.outbox),
            tab => self.pickers[tab.index() - 1].key(cx, key, &env, &mut self.outbox),
        }
    }

    fn key_down(&mut self, cx: &mut Ctx, key: &KeyEvent) -> bool {
        let mods = key.mods;
        if key.key == Key::Control || mods.ctrl != self.ctrl_held {
            self.ctrl_held = key.key == Key::Control || mods.ctrl;
            cx.request_paint();
            if key.key == Key::Control {
                return true;
            }
        }
        if key.key == Key::Escape {
            if self.dismiss_overlay() {
                cx.request_paint();
            } else if self.search.clear() {
                self.query_changed(cx);
            } else {
                self.outbox.push(PanelAction::Close);
            }
            return true;
        }
        if self.tab_has_overlay() {
            self.tab_key(cx, key);
            return true;
        }
        let query = !self.search.is_empty();
        match key.key {
            Key::Tab if !mods.alt => {
                self.switch_tab(cx, self.tab.step(!mods.shift));
                true
            }
            Key::Backspace => {
                if self.search.delete_backward(cx.gfx(), mods.ctrl) {
                    self.query_changed(cx);
                }
                true
            }
            Key::Char('A') if mods.ctrl && !mods.shift && !mods.alt => {
                self.search.select_all();
                cx.request_paint();
                true
            }
            Key::Left | Key::Right if query && mods.shift => {
                self.search.move_caret(cx.gfx(), key.key == Key::Right, mods.ctrl, false);
                cx.request_paint();
                true
            }
            Key::Delete if query && !(self.tab == Tab::Clipboard && self.search.caret_at_end()) => {
                if self.search.delete_forward(cx.gfx(), mods.ctrl) {
                    self.query_changed(cx);
                }
                true
            }
            _ => self.tab_key(cx, key),
        }
    }

    fn text_input(&mut self, cx: &mut Ctx, text: &str) -> bool {
        if self.tab_has_overlay() {
            self.dismiss_overlay();
        }
        if self.search.insert(text) {
            self.query_changed(cx);
        }
        true
    }

    fn in_drag_zone(&self, pos: PointF) -> bool {
        let on_tab = (0..Tab::ALL.len()).any(|i| self.tabs.item_rect(i).is_some_and(|r| r.contains(pos)));
        pos.y < CONTENT_TOP && !self.search.rect().contains(pos) && !self.gear.rect().contains(pos) && !on_tab
    }

    fn header_event(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        if !matches!(event, Event::Text(_) | Event::KeyDown(_)) {
            let response = self.search.event(cx, event);
            if response.action().is_some() {
                self.query_changed(cx);
                return true;
            }
            if matches!(event, Event::PointerDown(e) if self.search.rect().contains(e.pos)) {
                return true;
            }
        }
        if self.gear.event(cx, event).action().is_some() {
            self.outbox.push(PanelAction::OpenSettings);
            return true;
        }
        if let Some(index) = self.tabs.event(cx, event).action() {
            self.switch_tab(cx, Tab::from_index(index));
            return true;
        }
        match event {
            Event::PointerDown(e) if e.button == Some(MouseButton::Left) && self.in_drag_zone(e.pos) => {
                if let Some(rect) = cx.window_rect_px() {
                    self.drag = Some((e.screen_px, PointI::new(rect.x, rect.y)));
                    cx.capture_pointer();
                }
                true
            }
            _ => false,
        }
    }

    fn drag_event(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        let Some((start, origin)) = self.drag else { return false };
        match event {
            Event::PointerMove(e) => {
                let to = PointI::new(origin.x + e.screen_px.x - start.x, origin.y + e.screen_px.y - start.y);
                self.outbox.push(PanelAction::DragTo(to));
            }
            Event::PointerUp(_) | Event::PointerCancel => {
                self.drag = None;
                cx.release_pointer();
            }
            _ => {}
        }
        true
    }

    fn tab_event(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        let env = self.env();
        match self.tab {
            Tab::Clipboard => self.clipboard.event(cx, event, &env, &mut self.outbox),
            tab => {
                let handled = self.pickers[tab.index() - 1].event(cx, event, &env, &mut self.outbox);
                if let Some(PanelAction::SkinTone(tone)) = self.outbox.last() {
                    self.options.skin_tone = *tone;
                }
                handled
            }
        }
    }

    fn route(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        match event {
            Event::Timer(BLINK_TIMER) => {
                if self.presence.is_shown() {
                    cx.request_paint();
                    cx.set_timer(self.search.next_blink(), BLINK_TIMER);
                }
                return true;
            }
            Event::Timer(LABEL_TIMER | LONG_PRESS_TIMER) => return self.tab_event(cx, event),
            Event::Timer(_) => {
                let env = self.env();
                return self.clipboard.event(cx, event, &env, &mut self.outbox)
                    || self.pickers.iter_mut().any(|p| p.event(cx, event, &env, &mut self.outbox));
            }
            Event::Shown => {
                self.search.restart_blink();
                cx.set_timer(TextField::BLINK, BLINK_TIMER);
                return true;
            }
            Event::Resized => {
                self.size = cx.size();
                cx.request_paint();
                return true;
            }
            Event::KeyDown(key) => return self.key_down(cx, key),
            Event::KeyUp(key) if key.key == Key::Control => {
                self.ctrl_held = false;
                cx.request_paint();
                return true;
            }
            Event::Text(text) => return self.text_input(cx, text),
            _ => {}
        }
        if self.drag_event(cx, event) {
            return true;
        }
        if self.tab_has_overlay() {
            return self.tab_event(cx, event);
        }
        if self.header_event(cx, event) {
            return true;
        }
        self.tab_event(cx, event)
    }

    fn paint_tab(&mut self, cx: &mut Ctx, p: &mut Painter, tab: Tab, env: &Env) {
        match tab {
            Tab::Clipboard => {
                self.clipboard.paint(cx, p, env, &mut self.outbox);
                self.clipboard.paint_bar(cx, p, env);
            }
            tab => {
                let picker = &mut self.pickers[tab.index() - 1];
                picker.paint(cx, p, env);
                picker.paint_bar(cx, p, env);
            }
        }
    }

    fn paint_content(&mut self, cx: &mut Ctx, p: &mut Painter, env: &Env) {
        let theme = p.theme().clone();
        let bar = self.bar_rect();
        p.fill_rect(RectF::new(0.0, p.snap(bar.y), bar.w, p.px()), theme.separator);
        let switch = self.switch.as_ref().map(|s| (s.from, s.progress.get(), s.direction));
        match switch {
            Some((from, progress, direction)) if progress < 1.0 => {
                let tab = self.tab;
                p.layer(1.0 - progress, |p| p.translate(-TAB_SLIDE * direction * progress, 0.0, |p| self.paint_tab(cx, p, from, env)));
                p.layer(progress, |p| p.translate(TAB_SLIDE * direction * (1.0 - progress), 0.0, |p| self.paint_tab(cx, p, tab, env)));
            }
            _ => {
                self.switch = None;
                self.paint_tab(cx, p, self.tab, env);
            }
        }
        self.search.paint(p);
        self.gear.paint(p);
        self.tabs.paint(p);
        match self.tab {
            Tab::Clipboard => self.clipboard.paint_overlays(p),
            tab => self.pickers[tab.index() - 1].paint_overlays(p, env),
        }
    }
}

/// Preview hooks for galleries and the app's `--preview`: put the panel into states a pointer would.
impl Panel {
    pub fn preview_query(&mut self, query: &str) {
        self.search.set_text(query);
        self.search.force_caret(Some(true));
    }

    pub fn preview_caret(&mut self, visible: Option<bool>) {
        self.search.force_caret(visible);
    }

    pub fn preview_ctrl_held(&mut self, held: bool) {
        self.ctrl_held = held;
    }

    pub fn preview_hover_card(&mut self, id: Option<ClipId>) {
        self.clipboard.force_hover(id);
    }

    pub fn preview_select_card(&mut self, id: ClipId) {
        self.clipboard.force_select(id);
    }

    /// Opens the context menu of card `id` at `at` (DIP) with row `highlighted` lit.
    pub fn preview_menu(&mut self, gfx: &Gfx, id: ClipId, at: PointF, highlighted: Option<usize>) {
        self.layout(gfx);
        let env = self.env();
        self.clipboard.open_menu(gfx, id, at, &env, highlighted);
    }

    pub fn preview_confirm_clear(&mut self) {
        self.clipboard.set_confirming_clear(true);
    }

    /// Hovers the first cell showing `text` in a picker tab, optionally with its name label.
    pub fn preview_hover_cell(&mut self, gfx: &Gfx, tab: Tab, text: &str, label: bool) {
        self.layout(gfx);
        if let Some(picker) = self.picker_mut(tab)
            && let Some(cell) = picker.cell_of(gfx, text)
        {
            picker.force_hover(gfx, cell, label);
        }
    }

    pub fn preview_select_cell(&mut self, gfx: &Gfx, tab: Tab, index: usize) {
        self.layout(gfx);
        if let Some(picker) = self.picker_mut(tab) {
            picker.force_select(gfx, index);
        }
    }

    /// Opens the tone popover on the emoji showing `text`.
    pub fn preview_tones(&mut self, gfx: &Gfx, text: &str, highlighted: Option<usize>) {
        self.layout(gfx);
        let env = self.env();
        let picker = &mut self.pickers[0];
        if let Some(cell) = picker.cell_of(gfx, text) {
            picker.force_hover(gfx, cell, false);
            picker.open_tones(cell, &env, highlighted, true);
        }
    }

    pub fn preview_skin_picker(&mut self, gfx: &Gfx) {
        self.layout(gfx);
        let env = self.env();
        self.pickers[0].force_skin_picker(gfx, &env);
    }

    pub fn preview_scroll(&mut self, gfx: &Gfx, tab: Tab, offset: f32) {
        self.layout(gfx);
        let env = self.env();
        match self.picker_mut(tab) {
            Some(picker) => picker.force_scroll(gfx, offset),
            None => self.clipboard.force_scroll(gfx, &env, offset),
        }
    }

    pub fn preview_hover_gear(&mut self) {
        self.gear.force_state(true, false);
    }

    /// The actions the panel has produced since the last call (tests and previews; `cx.post` delivers them live).
    #[cfg(test)]
    pub(crate) fn take_posted(&mut self) -> Vec<PanelAction> {
        std::mem::take(&mut self.posted)
    }
}

impl View for Panel {
    fn event(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        if self.closing {
            return matches!(event, Event::KeyDown(_) | Event::Text(_));
        }
        self.size = cx.size();
        self.layout(cx.gfx());
        let handled = self.route(cx, event);
        self.flush(cx);
        handled
    }

    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter) {
        self.size = cx.size();
        self.layout(cx.gfx());
        let (opacity, scale, dy) = self.presence.transform();
        if opacity <= 0.001 {
            return;
        }
        let theme = p.theme().clone();
        let bounds = p.bounds();
        p.layer(opacity, |p| {
            p.fill_round_rect(bounds, WINDOW_RADIUS, style::glass_fill(&theme, self.backdrop.is_opaque()));
            p.hairline_round_rect(bounds, WINDOW_RADIUS, theme.hairline, true);
        });
        let env = self.env();
        let origin = PointF::new(bounds.w / 2.0, 0.0);
        p.layer(opacity, |p| p.translate(0.0, dy, |p| p.scale_around(scale, origin, |p| self.paint_content(cx, p, &env))));
        self.flush(cx);
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use tuck_core::{ClipContent, ClipItem, TextKind};
    use tuck_ui::{Gfx, KeyEvent, Modifiers, Theme};

    use super::*;
    use crate::PickerItem;

    fn cx(gfx: &Rc<Gfx>) -> Ctx {
        Ctx::offscreen(gfx.clone(), PANEL_SIZE, 1.0, Theme::dark(), 1.0e6)
    }

    fn key(key: Key, mods: Modifiers) -> Event {
        Event::KeyDown(KeyEvent { key, vk: 0, mods, repeat: false })
    }

    fn row(id: ClipId, text: &str, pinned: bool) -> ClipRow {
        let item = ClipItem {
            id,
            content: ClipContent::Text { text: text.into(), html: None, rtf: None },
            hash: id as u64,
            created_ms: 0,
            used_ms: 0,
            pinned,
            source: None,
            bytes: text.len() as u64,
        };
        ClipRow { item, kind: tuck_core::classify(text), thumbnail: None, app_icon: None, app_name: None }
    }

    fn panel(gfx: &Rc<Gfx>, tab: Tab) -> (Panel, Ctx) {
        let mut cx = cx(gfx);
        let mut panel = Panel::new(PanelOptions::default());
        panel.open(&mut cx, tab);
        panel.set_clips(&mut cx, vec![row(1, "pinned note", true), row(2, "hello", false), row(3, "world", false)], 3);
        let emoji = vec![
            PickerSection {
                title: "Smileys".into(),
                icon: Icon::Smile,
                items: ["😀", "😂", "🥰"].iter().map(|e| PickerItem::new(e, "face")).collect(),
            },
            PickerSection {
                title: "People".into(),
                icon: Icon::Hand,
                items: vec![PickerItem::new("👋", "waving hand").with_variants(
                    ["👋", "👋🏻", "👋🏼", "👋🏽", "👋🏾", "👋🏿"].iter().map(|s| s.to_string()).collect(),
                )],
            },
        ];
        panel.set_sections(&mut cx, Tab::Emoji, emoji);
        panel.take_posted();
        (panel, cx)
    }

    fn send(panel: &mut Panel, cx: &mut Ctx, event: Event) -> Vec<PanelAction> {
        panel.event(cx, &event);
        panel.take_posted()
    }

    #[test]
    fn typing_searches_and_escape_clears_then_closes() {
        let gfx = Gfx::new().unwrap();
        let (mut panel, mut cx) = panel(&gfx, Tab::Clipboard);
        let typed = send(&mut panel, &mut cx, Event::Text("he".into()));
        assert_eq!(typed, vec![PanelAction::QueryChanged { tab: Tab::Clipboard, query: "he".into() }]);
        send(&mut panel, &mut cx, key(Key::Backspace, Modifiers::CTRL));
        assert_eq!(panel.query(), "");
        send(&mut panel, &mut cx, Event::Text("x".into()));
        let cleared = send(&mut panel, &mut cx, key(Key::Escape, Modifiers::NONE));
        assert_eq!(cleared, vec![PanelAction::QueryChanged { tab: Tab::Clipboard, query: String::new() }]);
        assert_eq!(send(&mut panel, &mut cx, key(Key::Escape, Modifiers::NONE)), vec![PanelAction::Close]);
    }

    #[test]
    fn tab_keys_cycle_tabs() {
        let gfx = Gfx::new().unwrap();
        let (mut panel, mut cx) = panel(&gfx, Tab::Clipboard);
        assert_eq!(send(&mut panel, &mut cx, key(Key::Tab, Modifiers::NONE)), vec![PanelAction::TabChanged(Tab::Emoji)]);
        assert_eq!(send(&mut panel, &mut cx, key(Key::Tab, Modifiers::CTRL_SHIFT)), vec![PanelAction::TabChanged(Tab::Clipboard)]);
        assert_eq!(send(&mut panel, &mut cx, key(Key::Tab, Modifiers::SHIFT)), vec![PanelAction::TabChanged(Tab::Symbols)]);
    }

    #[test]
    fn clipboard_keys_paste_copy_pin_and_delete() {
        let gfx = Gfx::new().unwrap();
        let (mut panel, mut cx) = panel(&gfx, Tab::Clipboard);
        assert_eq!(send(&mut panel, &mut cx, key(Key::Enter, Modifiers::NONE)), vec![PanelAction::Paste { id: 1, plain: false }]);
        send(&mut panel, &mut cx, key(Key::Down, Modifiers::NONE));
        assert_eq!(send(&mut panel, &mut cx, key(Key::Enter, Modifiers::SHIFT)), vec![PanelAction::Paste { id: 2, plain: true }]);
        assert_eq!(send(&mut panel, &mut cx, key(Key::Enter, Modifiers::CTRL)), vec![PanelAction::Copy { id: 2 }]);
        assert_eq!(send(&mut panel, &mut cx, key(Key::Char('3'), Modifiers::CTRL)), vec![PanelAction::Paste { id: 3, plain: false }]);
        assert_eq!(send(&mut panel, &mut cx, key(Key::Char('P'), Modifiers::CTRL)), vec![PanelAction::SetPinned { id: 2, pinned: true }]);
        assert_eq!(send(&mut panel, &mut cx, key(Key::Delete, Modifiers::NONE)), vec![PanelAction::Delete { id: 2 }]);
        assert_eq!(send(&mut panel, &mut cx, key(Key::Enter, Modifiers::NONE)), vec![PanelAction::Paste { id: 3, plain: false }]);
    }

    #[test]
    fn delete_edits_the_query_unless_the_caret_is_at_its_end() {
        let gfx = Gfx::new().unwrap();
        let (mut panel, mut cx) = panel(&gfx, Tab::Clipboard);
        send(&mut panel, &mut cx, Event::Text("ab".into()));
        send(&mut panel, &mut cx, key(Key::Left, Modifiers::SHIFT));
        let edited = send(&mut panel, &mut cx, key(Key::Delete, Modifiers::NONE));
        assert_eq!(edited, vec![PanelAction::QueryChanged { tab: Tab::Clipboard, query: "a".into() }]);
        assert_eq!(send(&mut panel, &mut cx, key(Key::Delete, Modifiers::NONE)), vec![PanelAction::Delete { id: 1 }]);
    }

    #[test]
    fn emoji_arrows_move_and_enter_inserts_with_the_tone() {
        let gfx = Gfx::new().unwrap();
        let (mut panel, mut cx) = panel(&gfx, Tab::Emoji);
        let options = PanelOptions { skin_tone: SkinTone::Medium, ..PanelOptions::default() };
        panel.set_options(&mut cx, options);
        send(&mut panel, &mut cx, key(Key::Right, Modifiers::NONE));
        let inserted = send(&mut panel, &mut cx, key(Key::Enter, Modifiers::NONE));
        assert_eq!(inserted, vec![PanelAction::Insert { tab: Tab::Emoji, text: "😂".into(), close: true }]);
        send(&mut panel, &mut cx, key(Key::Down, Modifiers::NONE));
        let toned = send(&mut panel, &mut cx, key(Key::Enter, Modifiers::NONE));
        assert_eq!(toned, vec![PanelAction::Insert { tab: Tab::Emoji, text: "👋🏽".into(), close: true }]);
    }

    #[test]
    fn control_shows_badges_until_released() {
        let gfx = Gfx::new().unwrap();
        let (mut panel, mut cx) = panel(&gfx, Tab::Clipboard);
        send(&mut panel, &mut cx, key(Key::Control, Modifiers::CTRL));
        assert!(panel.ctrl_held);
        panel.event(&mut cx, &Event::KeyUp(KeyEvent { key: Key::Control, vk: 0x11, mods: Modifiers::NONE, repeat: false }));
        assert!(!panel.ctrl_held);
    }

    #[test]
    fn closing_finishes_and_ignores_input() {
        let gfx = Gfx::new().unwrap();
        let (mut panel, mut cx) = panel(&gfx, Tab::Clipboard);
        panel.close(&mut cx);
        assert!(send(&mut panel, &mut cx, key(Key::Enter, Modifiers::NONE)).is_empty());
        tuck_ui::anim::with_clock(tuck_ui::anim::now() + 1.0, || assert!(panel.is_closing_done()));
    }

    #[test]
    fn classify_feeds_card_kinds() {
        assert_eq!(row(9, "https://example.com", false).kind, TextKind::Url);
    }
}
