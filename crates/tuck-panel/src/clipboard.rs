//! The Clipboard tab: Pinned and Recent cards for every text kind, images and files; hover buttons, the context
//! menu, the selection ring, empty / no-results / paused states and the bottom bar with the inline clear
//! confirmation. Cards move with layout springs: deleting collapses a card, pinning slides it into Pinned.

use std::collections::HashSet;
use std::rc::Rc;

use tuck_core::{ClipContent, ClipId, TextKind};
use tuck_ui::widgets::{Button, ButtonStyle, IconButton, Menu, MenuItem, Response, ScrollView};
use tuck_ui::{
    Animated, Bitmap, Color, Ctx, Event, FontFamily, Gfx, Icon, Interpolation, Key, KeyEvent, MouseButton, Painter,
    PointF, RectF, SizeF, TextAlign, TextStyle, Weight,
};

use crate::format;
use crate::panel::Env;
use crate::style::{self, CLIPBOARD_SCROLL_TIMER, PADDING, SECTION_HEADER};
use crate::{ClipRow, PanelAction};

const CARD_RADIUS: f32 = 10.0;
const CARD_GAP: f32 = 8.0;
const PAD_X: f32 = 12.0;
const PAD_Y: f32 = 10.0;
const LINE: f32 = 16.0;
const META_GAP: f32 = 8.0;
const META: f32 = 16.0;
const MAX_LINES: usize = 4;
const PREVIEW_CHARS: usize = 600;
const IMAGE_MAX_HEIGHT: f32 = 110.0;
const IMAGE_RADIUS: f32 = 6.0;
const LEAD_ICON: f32 = 16.0;
const LEAD_WIDTH: f32 = 24.0;
const SWATCH: f32 = 20.0;
const APP_ICON: f32 = 14.0;
const BANNER: f32 = 44.0;
const HOVER_BUTTON: f32 = 26.0;
const PLATE_PAD: f32 = 2.0;
const SECTION_SPACING: f32 = 4.0;
const BOTTOM_PADDING: f32 = 12.0;
const REVEAL_MARGIN: f32 = 8.0;

fn text_style() -> TextStyle {
    TextStyle::body().wrap().line_height(LINE)
}

fn mono_style() -> TextStyle {
    TextStyle::new(12.0).family(FontFamily::Mono).wrap().line_height(LINE)
}

fn meta_style() -> TextStyle {
    TextStyle::caption()
}

#[derive(Clone, Debug, PartialEq)]
enum Body {
    Text { text: String, mono: bool, lines: u32 },
    Link { host: String, url: String },
    Line { icon: Icon, text: String },
    Swatch { color: Color, value: String },
    Image { fit: SizeF, pixels: (u32, u32) },
    Files { icon: Icon, names: Vec<String>, more: usize },
}

impl Body {
    fn height(&self) -> f32 {
        match self {
            Body::Text { lines, .. } => *lines as f32 * LINE,
            Body::Link { .. } => 2.0 * LINE,
            Body::Line { .. } => LINE,
            Body::Swatch { .. } => SWATCH,
            Body::Image { fit, .. } => fit.h,
            Body::Files { names, more, .. } => (names.len() + usize::from(*more > 0)) as f32 * LINE,
        }
    }
}

/// `text` cut to at most `MAX_LINES` wrapped lines at `width`, with an ellipsis when cut.
fn fit_lines(gfx: &Gfx, text: &str, style: &TextStyle, width: f32) -> (String, u32) {
    let Ok(layout) = gfx.text_layout(text, style, Some(width)) else { return (text.to_string(), 1) };
    if layout.line_count as usize <= MAX_LINES {
        return (text.to_string(), layout.line_count.max(1));
    }
    let end = layout.hit_test(PointF::new(width, (MAX_LINES as f32 - 0.5) * LINE)) as usize;
    let mut cut = text.char_indices().map(|(i, _)| i).nth(text.chars().take(end).count()).unwrap_or(text.len());
    for _ in 0..48 {
        let candidate = format!("{}…", text[..cut].trim_end());
        let fits = gfx.text_layout(&candidate, style, Some(width)).is_ok_and(|l| l.line_count as usize <= MAX_LINES);
        if fits || cut == 0 {
            return (candidate, MAX_LINES as u32);
        }
        cut = text[..cut].char_indices().next_back().map_or(0, |(i, _)| i);
    }
    (text.to_string(), MAX_LINES as u32)
}

fn color_of(rgba: tuck_core::Rgba) -> Color {
    Color::rgba8(rgba.r, rgba.g, rgba.b, rgba.a as f32 / 255.0)
}

fn body_for(gfx: &Gfx, row: &ClipRow, width: f32) -> Body {
    match &row.item.content {
        ClipContent::Text { text, .. } => {
            let single = text.trim();
            match row.kind {
                TextKind::Url => {
                    let url = single.lines().next().unwrap_or(single).to_string();
                    Body::Link { host: format::url_host(&url).to_string(), url }
                }
                TextKind::Email => Body::Line { icon: Icon::Mail, text: single.to_string() },
                TextKind::Path => {
                    let style = TextStyle::body();
                    Body::Line { icon: Icon::Folder, text: gfx.ellipsize_middle(single, &style, width - LEAD_WIDTH) }
                }
                TextKind::Color(rgba) => Body::Swatch { color: color_of(rgba), value: single.to_string() },
                TextKind::Code | TextKind::Plain => {
                    let mono = row.kind == TextKind::Code;
                    let style = if mono { mono_style() } else { text_style() };
                    let preview = format::preview_text(text, if mono { MAX_LINES } else { 12 }, PREVIEW_CHARS);
                    let (text, lines) = fit_lines(gfx, &preview, &style, width);
                    Body::Text { text, mono, lines }
                }
            }
        }
        ClipContent::Image { width: w, height: h, .. } => {
            let (w_px, h_px) = ((*w).max(1) as f32, (*h).max(1) as f32);
            let scale = (width / w_px).min(IMAGE_MAX_HEIGHT / h_px).min(2.0);
            let fit = SizeF::new((w_px * scale).round().max(24.0).min(width), (h_px * scale).round().clamp(24.0, IMAGE_MAX_HEIGHT));
            Body::Image { fit, pixels: (*w, *h) }
        }
        ClipContent::Files { paths } => {
            let (names, more) = format::file_summary(paths, 3);
            let icon = match paths.as_slice() {
                [only] if format::looks_like_folder(only) => Icon::Folder,
                [_] => Icon::File,
                _ => Icon::Files,
            };
            Body::Files { icon, names, more }
        }
    }
}

struct Card {
    row: ClipRow,
    body: Body,
    body_width: f32,
    y: Animated<f32>,
    height: Animated<f32>,
    opacity: Animated<f32>,
    hover: Animated<f32>,
    leaving: bool,
}

impl Card {
    fn new(row: ClipRow) -> Self {
        Self {
            row,
            body: Body::Line { icon: Icon::File, text: String::new() },
            body_width: -1.0,
            y: Animated::new(0.0),
            height: Animated::new(0.0),
            opacity: Animated::fade(1.0),
            hover: Animated::snappy(0.0),
            leaving: false,
        }
    }

    fn id(&self) -> ClipId {
        self.row.item.id
    }

    fn pinned(&self) -> bool {
        self.row.item.pinned
    }

    fn full_height(&self) -> f32 {
        PAD_Y + self.body.height() + META_GAP + META + PAD_Y
    }

    fn rect(&self, width: f32) -> RectF {
        RectF::new(PADDING, self.y.get(), width - 2.0 * PADDING, self.height.get())
    }

    fn target_rect(&self, width: f32) -> RectF {
        RectF::new(PADDING, self.y.target(), width - 2.0 * PADDING, self.height.target())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Paste { plain: bool },
    Copy,
    TogglePin,
    CopyText,
    EditInGlint,
    ShowInExplorer,
    Delete,
}

struct CardMenu {
    id: ClipId,
    menu: Menu,
    commands: Vec<Option<Command>>,
}

pub(crate) struct ClipboardTab {
    cards: Vec<Card>,
    total: usize,
    headers: [Option<f32>; 2],
    banner: Option<RectF>,
    scroll: ScrollView,
    viewport: RectF,
    bar: RectF,
    width: f32,
    dirty: bool,
    animate_layout: bool,
    selected: Option<ClipId>,
    ring: Animated<RectF>,
    ring_opacity: Animated<f32>,
    hovered: Option<ClipId>,
    pressed: Option<ClipId>,
    pin: IconButton,
    delete: IconButton,
    menu: Option<CardMenu>,
    confirming_clear: bool,
    clear: Button,
    cancel: Button,
    confirm: Button,
    resume: Button,
    banner_resume: Button,
    requested: HashSet<ClipId>,
}

impl ClipboardTab {
    pub fn new() -> Self {
        let small = |icon| IconButton::new(icon).icon_size(15.0);
        Self {
            cards: Vec::new(),
            total: 0,
            headers: [None, None],
            banner: None,
            scroll: ScrollView::new(CLIPBOARD_SCROLL_TIMER),
            viewport: RectF::default(),
            bar: RectF::default(),
            width: 0.0,
            dirty: true,
            animate_layout: false,
            selected: None,
            ring: Animated::new(RectF::default()),
            ring_opacity: Animated::fade(0.0),
            hovered: None,
            pressed: None,
            pin: small(Icon::Pin).tooltip("Pin", Some("Ctrl+P")),
            delete: small(Icon::Trash).tooltip("Delete", Some("Del")),
            menu: None,
            confirming_clear: false,
            clear: Button::new("Clear", ButtonStyle::PlainDestructive),
            cancel: Button::new("Cancel", ButtonStyle::Plain),
            confirm: Button::new("Clear", ButtonStyle::Destructive),
            resume: Button::new("Resume", ButtonStyle::Plain),
            banner_resume: Button::new("Resume", ButtonStyle::Plain),
            requested: HashSet::new(),
        }
    }

    /// Replaces the rows (already filtered and ordered by the app). Cards that disappear collapse; new ones fade in.
    pub fn set_rows(&mut self, rows: Vec<ClipRow>, total: usize, animate: bool) {
        self.total = total;
        let keep: HashSet<ClipId> = rows.iter().map(|r| r.item.id).collect();
        let old_order: Vec<ClipId> = self.cards.iter().map(Card::id).collect();
        let mut previous: Vec<Card> = std::mem::take(&mut self.cards);
        let mut cards: Vec<Card> = Vec::with_capacity(rows.len());
        for row in rows {
            let card = match previous.iter().position(|c| c.id() == row.item.id) {
                Some(index) => {
                    let mut card = previous.remove(index);
                    if card.row.item.content != row.item.content || card.row.kind != row.kind {
                        card.body_width = -1.0;
                    }
                    let thumbnail = card.row.thumbnail.take();
                    card.row = row;
                    if card.row.thumbnail.is_none() {
                        card.row.thumbnail = thumbnail;
                    }
                    card.leaving = false;
                    card.opacity.set(1.0);
                    card
                }
                None => {
                    let mut card = Card::new(row);
                    if animate {
                        card.opacity.snap(0.0);
                        card.opacity.set(1.0);
                    }
                    card
                }
            };
            cards.push(card);
        }
        if animate {
            for mut card in previous.into_iter().filter(|c| !keep.contains(&c.id())) {
                card.leaving = true;
                card.opacity.set(0.0);
                let old_index = old_order.iter().position(|id| *id == card.id()).unwrap_or(0);
                let before = old_order[..old_index].iter().rev().find_map(|id| cards.iter().position(|c| c.id() == *id));
                cards.insert(before.map_or(0, |i| i + 1), card);
            }
        }
        self.cards = cards;
        self.requested.retain(|id| keep.contains(id));
        if self.menu.as_ref().is_some_and(|m| !keep.contains(&m.id)) {
            self.menu = None;
        }
        if self.selected.is_none_or(|id| !keep.contains(&id)) {
            self.selected = self.visible_ids().first().copied();
        }
        self.dirty = true;
        self.animate_layout = animate;
    }

    pub fn set_thumbnail(&mut self, id: ClipId, bitmap: Rc<Bitmap>) -> bool {
        match self.cards.iter_mut().find(|c| c.id() == id) {
            Some(card) => {
                card.row.thumbnail = Some(bitmap);
                true
            }
            None => false,
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.animate_layout = true;
    }

    /// Fresh state for a newly opened panel: top of the list, first card selected, nothing open.
    pub fn reset(&mut self) {
        self.cards.retain(|c| !c.leaving);
        self.scroll.snap_to(0.0);
        self.selected = self.visible_ids().first().copied();
        self.hovered = None;
        self.pressed = None;
        self.menu = None;
        self.confirming_clear = false;
        self.dirty = true;
        self.animate_layout = false;
    }

    pub fn set_frame(&mut self, viewport: RectF, bar: RectF) {
        self.viewport = viewport;
        self.bar = bar;
        self.scroll.set_viewport(viewport);
    }

    fn visible_ids(&self) -> Vec<ClipId> {
        self.cards.iter().filter(|c| !c.leaving).map(Card::id).collect()
    }

    fn card(&self, id: ClipId) -> Option<&Card> {
        self.cards.iter().find(|c| c.id() == id)
    }

    fn card_mut(&mut self, id: ClipId) -> Option<&mut Card> {
        self.cards.iter_mut().find(|c| c.id() == id)
    }

    pub fn has_overlay(&self) -> bool {
        self.menu.as_ref().is_some_and(|m| m.menu.is_open()) || self.confirming_clear
    }

    /// Esc while a menu or the clear confirmation is up dismisses it.
    pub fn dismiss_overlay(&mut self) -> bool {
        if let Some(menu) = self.menu.as_mut().filter(|m| m.menu.is_open()) {
            menu.menu.close();
            return true;
        }
        std::mem::take(&mut self.confirming_clear)
    }

    fn ensure_layout(&mut self, gfx: &Gfx, env: &Env) {
        let width = self.viewport.w;
        if !self.dirty && (width - self.width).abs() < 0.01 {
            return;
        }
        self.width = width;
        self.dirty = false;
        let inner = width - 2.0 * PADDING - 2.0 * PAD_X;
        self.cards.sort_by_key(|c| !c.pinned());
        let mut y = 0.0;
        self.banner = env.options.paused.then(|| RectF::new(PADDING, 0.0, width - 2.0 * PADDING, BANNER));
        if self.banner.is_some() {
            y += BANNER + CARD_GAP;
        }
        let pinned = self.cards.iter().any(|c| c.pinned() && !c.leaving);
        let recent = self.cards.iter().any(|c| !c.pinned() && !c.leaving);
        let animate = self.animate_layout;
        self.headers = [None, None];
        if pinned {
            self.headers[0] = Some(y);
            y += SECTION_HEADER;
        }
        let mut in_recent = false;
        for card in &mut self.cards {
            if pinned && recent && !card.pinned() && !in_recent {
                in_recent = true;
                y += SECTION_SPACING;
                self.headers[1] = Some(y);
                y += SECTION_HEADER;
            }
            if (card.body_width - inner).abs() > 0.01 {
                card.body = body_for(gfx, &card.row, inner);
                card.body_width = inner;
            }
            let height = if card.leaving { 0.0 } else { card.full_height() };
            let fresh = card.height.target() == 0.0 && !card.leaving;
            if animate && !fresh {
                card.y.set(y);
                card.height.set(height);
            } else {
                card.y.snap(y);
                card.height.snap(height);
            }
            if !card.leaving {
                y += height + CARD_GAP;
            }
        }
        self.animate_layout = true;
        self.scroll.set_content_height(if self.cards.is_empty() { y } else { y - CARD_GAP + BOTTOM_PADDING });
        self.sync_ring(animate);
    }

    fn sync_ring(&mut self, animate: bool) {
        let target = self.selected.and_then(|id| self.card(id)).filter(|c| !c.leaving).map(|c| c.target_rect(self.width));
        match target {
            Some(rect) if animate && self.ring_opacity.target() > 0.0 => self.ring.set(rect),
            Some(rect) if animate => {
                self.ring.snap(rect);
                self.ring_opacity.set(1.0);
            }
            Some(rect) => {
                self.ring.snap(rect);
                self.ring_opacity.snap(1.0);
            }
            None => self.ring_opacity.set(0.0),
        }
    }

    fn select(&mut self, cx: &mut Ctx, id: ClipId) {
        self.selected = Some(id);
        self.sync_ring(true);
        if let Some(rect) = self.card(id).map(|c| c.target_rect(self.width)) {
            let top = if self.visible_ids().first() == Some(&id) { 0.0 } else { rect.y };
            self.scroll.reveal(cx, top, rect.bottom(), REVEAL_MARGIN + self.sticky_height());
        }
        cx.request_paint();
    }

    fn sticky_height(&self) -> f32 {
        if self.headers[0].is_some() { SECTION_HEADER } else { 0.0 }
    }

    /// Moves the keyboard selection by `delta` cards (clamped).
    fn step_selection(&mut self, cx: &mut Ctx, delta: isize) {
        let ids = self.visible_ids();
        if ids.is_empty() {
            return;
        }
        let current = self.selected.and_then(|id| ids.iter().position(|i| *i == id)).unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, ids.len() as isize - 1) as usize;
        self.select(cx, ids[next]);
    }

    fn page_delta(&self) -> isize {
        let typical = self.cards.iter().find(|c| !c.leaving).map_or(80.0, |c| c.full_height() + CARD_GAP);
        ((self.viewport.h / typical).floor() as isize).max(1)
    }

    fn selected_card(&self) -> Option<&Card> {
        self.selected.and_then(|id| self.card(id)).filter(|c| !c.leaving)
    }

    /// Keyboard model for this tab (the panel routes text editing first).
    pub fn key(&mut self, cx: &mut Ctx, key: &KeyEvent, env: &Env, out: &mut Vec<PanelAction>) -> bool {
        if let Some(menu) = self.menu.as_mut().filter(|m| m.menu.is_open()) {
            if let Response::Action(index) = menu.menu.event(cx, &Event::KeyDown(key.clone())) {
                let (id, command) = (menu.id, menu.commands.get(index).copied().flatten());
                if let Some(command) = command {
                    self.run(cx, id, command, env, out);
                }
            }
            cx.request_paint();
            return true;
        }
        let mods = key.mods;
        let plain_default = env.options.plain_by_default;
        match key.key {
            Key::Up | Key::Down if !mods.ctrl && !mods.alt => self.step_selection(cx, if key.key == Key::Up { -1 } else { 1 }),
            Key::PageUp => self.step_selection(cx, -self.page_delta()),
            Key::PageDown => self.step_selection(cx, self.page_delta()),
            Key::Home => self.step_selection(cx, isize::MIN / 2),
            Key::End => self.step_selection(cx, isize::MAX / 2),
            Key::Enter => {
                let Some(id) = self.selected_card().map(Card::id) else { return true };
                let command = if mods.ctrl { Command::Copy } else { Command::Paste { plain: plain_default != mods.shift } };
                self.run(cx, id, command, env, out);
            }
            Key::Delete => {
                let Some(id) = self.selected_card().map(Card::id) else { return false };
                self.run(cx, id, Command::Delete, env, out);
            }
            Key::Char('P') if mods.ctrl && !mods.shift && !mods.alt => {
                let Some(id) = self.selected_card().map(Card::id) else { return true };
                self.run(cx, id, Command::TogglePin, env, out);
            }
            Key::Char(digit @ '1'..='9') if mods.ctrl && !mods.alt => {
                let index = digit as usize - '1' as usize;
                if let Some(&id) = self.visible_ids().get(index) {
                    self.run(cx, id, Command::Paste { plain: plain_default != mods.shift }, env, out);
                }
            }
            _ => return false,
        }
        true
    }

    fn run(&mut self, cx: &mut Ctx, id: ClipId, command: Command, env: &Env, out: &mut Vec<PanelAction>) {
        match command {
            Command::Paste { plain } => out.push(PanelAction::Paste { id, plain }),
            Command::Copy => out.push(PanelAction::Copy { id }),
            Command::CopyText => out.push(PanelAction::CopyImageText { id }),
            Command::EditInGlint => out.push(PanelAction::EditInGlint { id }),
            Command::ShowInExplorer => out.push(PanelAction::ShowInExplorer { id }),
            Command::TogglePin => {
                let Some(card) = self.card_mut(id) else { return };
                card.row.item.pinned = !card.row.item.pinned;
                let pinned = card.row.item.pinned;
                out.push(PanelAction::SetPinned { id, pinned });
                self.mark_dirty();
                self.ensure_layout(cx.gfx(), env);
                if self.selected == Some(id) {
                    self.select(cx, id);
                }
            }
            Command::Delete => {
                let ids = self.visible_ids();
                let position = ids.iter().position(|i| *i == id);
                let Some(card) = self.card_mut(id) else { return };
                card.leaving = true;
                card.opacity.set(0.0);
                out.push(PanelAction::Delete { id });
                if self.selected == Some(id) {
                    let next = position.and_then(|p| ids.get(p + 1).or_else(|| p.checked_sub(1).and_then(|q| ids.get(q))));
                    self.selected = next.copied();
                }
                if self.hovered == Some(id) {
                    self.hovered = None;
                }
                self.mark_dirty();
                self.ensure_layout(cx.gfx(), env);
            }
        }
        cx.request_paint();
    }

    fn menu_for(&self, card: &Card, env: &Env) -> (Vec<MenuItem>, Vec<Option<Command>>) {
        let plain_default = env.options.plain_by_default;
        let mut items = Vec::new();
        let mut commands = Vec::new();
        let mut add = |item: MenuItem, command: Option<Command>| {
            items.push(item);
            commands.push(command);
        };
        add(MenuItem::new("Paste").icon(Icon::Clipboard).shortcut("Enter"), Some(Command::Paste { plain: plain_default }));
        let has_text = !matches!(card.row.item.content, ClipContent::Image { .. });
        if has_text {
            let label = if plain_default { "Paste with formatting" } else { "Paste as plain text" };
            add(MenuItem::new(label).icon(Icon::Type).shortcut("Shift+Enter"), Some(Command::Paste { plain: !plain_default }));
        }
        add(MenuItem::new("Copy").icon(Icon::Copy).shortcut("Ctrl+Enter"), Some(Command::Copy));
        let pin = if card.pinned() { MenuItem::new("Unpin").icon(Icon::PinOff) } else { MenuItem::new("Pin").icon(Icon::Pin) };
        add(pin.shortcut("Ctrl+P"), Some(Command::TogglePin));
        match card.row.item.content {
            ClipContent::Image { .. } => {
                add(MenuItem::separator(), None);
                add(MenuItem::new("Copy text from image").icon(Icon::ScanText), Some(Command::CopyText));
                if env.options.glint_available {
                    add(MenuItem::new("Edit in Glint").icon(Icon::Pencil), Some(Command::EditInGlint));
                }
            }
            ClipContent::Files { .. } => {
                add(MenuItem::separator(), None);
                add(MenuItem::new("Show in Explorer").icon(Icon::Folder), Some(Command::ShowInExplorer));
            }
            ClipContent::Text { .. } => {}
        }
        add(MenuItem::separator(), None);
        add(MenuItem::new("Delete").icon(Icon::Trash).shortcut("Del").destructive(), Some(Command::Delete));
        (items, commands)
    }

    /// Opens the context menu of card `id` at `at` (window DIP). `highlighted` is for previews.
    pub fn open_menu(&mut self, gfx: &Gfx, id: ClipId, at: PointF, env: &Env, highlighted: Option<usize>) {
        let Some(card) = self.card(id) else { return };
        let (items, commands) = self.menu_for(card, env);
        let mut menu = Menu::new(items).opaque();
        let anchor = RectF::new(at.x, at.y, 1.0, 1.0);
        match highlighted {
            Some(row) => menu.force_open(gfx, anchor, env.size, Some(row)),
            None => menu.open(gfx, anchor, env.size),
        }
        self.menu = Some(CardMenu { id, menu, commands });
        self.selected = Some(id);
        self.sync_ring(false);
    }

    /// Window-space rect of a card right now.
    fn window_rect(&self, card: &Card) -> RectF {
        self.scroll.to_window(card.rect(self.width))
    }

    fn card_at(&self, pos: PointF) -> Option<ClipId> {
        if !self.viewport.contains(pos) {
            return None;
        }
        let content = self.scroll.to_content(pos);
        self.cards.iter().filter(|c| !c.leaving).find(|c| c.rect(self.width).contains(content)).map(Card::id)
    }

    fn set_hovered(&mut self, cx: &mut Ctx, id: Option<ClipId>) {
        if self.hovered == id {
            return;
        }
        if let Some(card) = self.hovered.and_then(|old| self.card_mut(old)) {
            card.hover.set(0.0);
        }
        self.hovered = id;
        if let Some(card) = id.and_then(|new| self.card_mut(new)) {
            card.hover.set(1.0);
        }
        cx.request_paint();
    }

    /// Previews: hover a card without a pointer.
    pub fn force_hover(&mut self, id: Option<ClipId>) {
        self.hovered = id;
        for card in &mut self.cards {
            card.hover.snap(if Some(card.id()) == id { 1.0 } else { 0.0 });
        }
    }

    pub fn force_select(&mut self, id: ClipId) {
        self.selected = Some(id);
        self.dirty = true;
        self.animate_layout = false;
    }

    pub fn set_confirming_clear(&mut self, confirming: bool) {
        self.confirming_clear = confirming;
    }

    fn hover_plate(&self, card_rect: RectF) -> RectF {
        let w = 2.0 * HOVER_BUTTON + 2.0 * PLATE_PAD;
        RectF::new(card_rect.right() - 6.0 - w, card_rect.y + 6.0, w, HOVER_BUTTON + 2.0 * PLATE_PAD)
    }

    /// Positions the pin/delete buttons on the hovered card; None when they are hidden.
    fn place_hover_buttons(&mut self, env: &Env) -> Option<ClipId> {
        if env.ctrl_held || self.menu.as_ref().is_some_and(|m| m.menu.is_open()) {
            return None;
        }
        let id = self.hovered?;
        let card = self.card(id).filter(|c| !c.leaving)?;
        let pinned = card.pinned();
        let plate = self.hover_plate(self.window_rect(card));
        let sticky = if self.scroll.offset() > 0.5 { self.sticky_height() } else { 0.0 };
        if plate.y < self.viewport.y + sticky || plate.bottom() > self.viewport.bottom() {
            return None;
        }
        self.pin.icon = if pinned { Icon::PinOff } else { Icon::Pin };
        self.pin.tooltip = Some(if pinned { "Unpin" } else { "Pin" }.to_string());
        self.pin.set_rect(RectF::new(plate.x + PLATE_PAD, plate.y + PLATE_PAD, HOVER_BUTTON, HOVER_BUTTON));
        self.delete.set_rect(RectF::new(plate.x + PLATE_PAD + HOVER_BUTTON, plate.y + PLATE_PAD, HOVER_BUTTON, HOVER_BUTTON));
        Some(id)
    }

    /// Scrolls to `offset` once the layout exists (previews).
    pub fn force_scroll(&mut self, gfx: &Gfx, env: &Env, offset: f32) {
        self.ensure_layout(gfx, env);
        self.scroll.snap_to(offset);
        self.scroll.force_scrollbar(1.0);
    }

    fn has_unpinned(&self) -> bool {
        self.cards.iter().any(|c| !c.leaving && !c.pinned())
    }

    fn layout_bar(&mut self, gfx: &Gfx) {
        let bar = self.bar;
        let center = bar.center().y;
        let place = |button: &mut Button, right: f32, height: f32| {
            let w = button.preferred_size(gfx).w;
            button.set_rect(RectF::new(right - w, center - height / 2.0, w, height));
            right - w
        };
        let right = bar.right() - PADDING + 6.0;
        place(&mut self.clear, right, 28.0);
        let confirm_left = place(&mut self.confirm, bar.right() - PADDING, 24.0);
        place(&mut self.cancel, confirm_left - 6.0, 24.0);
        let paused_width = gfx.measure_text("Paused", &style::bar_label_style()).w;
        let resume_w = self.resume.preferred_size(gfx).w;
        self.resume.set_rect(RectF::new(bar.x + PADDING + 18.0 + paused_width + 2.0, center - 14.0, resume_w, 28.0));
    }

    /// Positions the banner's Resume button (it scrolls with the content); false without a banner.
    fn place_banner_resume(&mut self, gfx: &Gfx) -> bool {
        let Some(banner) = self.banner else { return false };
        let rect = self.scroll.to_window(banner);
        let w = self.banner_resume.preferred_size(gfx).w;
        self.banner_resume.set_rect(RectF::new(rect.right() - 8.0 - w, rect.center().y - 14.0, w, 28.0));
        true
    }

    fn bar_event(&mut self, cx: &mut Ctx, event: &Event, env: &Env, out: &mut Vec<PanelAction>) -> bool {
        self.layout_bar(cx.gfx());
        let has_unpinned = self.has_unpinned();
        if self.confirming_clear {
            if self.cancel.event(cx, event).action().is_some() {
                self.confirming_clear = false;
                cx.request_paint();
                return true;
            }
            if self.confirm.event(cx, event).action().is_some() {
                self.confirming_clear = false;
                for card in self.cards.iter_mut().filter(|c| !c.pinned()) {
                    card.leaving = true;
                    card.opacity.set(0.0);
                }
                out.push(PanelAction::ClearUnpinned);
                self.mark_dirty();
                self.ensure_layout(cx.gfx(), env);
                cx.request_paint();
                return true;
            }
            if matches!(event, Event::PointerDown(e) if !self.bar.contains(e.pos)) {
                self.confirming_clear = false;
                cx.request_paint();
            }
        } else if has_unpinned {
            let was = self.clear.is_hovered();
            if self.clear.event(cx, event).action().is_some() {
                self.confirming_clear = true;
                cx.request_paint();
                return true;
            }
            if was != self.clear.is_hovered() {
                cx.request_paint();
            }
        }
        if env.options.paused && self.resume.event(cx, event).action().is_some() {
            out.push(PanelAction::SetPaused(false));
            return true;
        }
        matches!(event, Event::PointerDown(e) | Event::PointerUp(e) if self.bar.contains(e.pos))
    }

    pub fn event(&mut self, cx: &mut Ctx, event: &Event, env: &Env, out: &mut Vec<PanelAction>) -> bool {
        self.ensure_layout(cx.gfx(), env);
        if let Some(menu) = self.menu.as_mut().filter(|m| m.menu.is_open()) {
            if let Response::Action(index) = menu.menu.event(cx, event) {
                let (id, command) = (menu.id, menu.commands.get(index).copied().flatten());
                if let Some(command) = command {
                    self.run(cx, id, command, env, out);
                }
            }
            cx.request_paint();
            return true;
        }
        if self.bar_event(cx, event, env, out) {
            return true;
        }
        if self.place_banner_resume(cx.gfx()) && self.banner_resume.event(cx, event).action().is_some() {
            out.push(PanelAction::SetPaused(false));
            return true;
        }
        if self.place_hover_buttons(env).is_some() {
            let hovered = self.hovered;
            if self.pin.event(cx, event).action().is_some() {
                if let Some(id) = hovered {
                    self.run(cx, id, Command::TogglePin, env, out);
                }
                return true;
            }
            if self.delete.event(cx, event).action().is_some() {
                if let Some(id) = hovered {
                    self.run(cx, id, Command::Delete, env, out);
                }
                return true;
            }
            if self.pin.is_pressed() || self.delete.is_pressed() {
                return true;
            }
        }
        if self.scroll.event(cx, event).consumed() {
            if let Event::Wheel(w) = event {
                let hovered = self.card_at(w.pos);
                self.set_hovered(cx, hovered);
            }
            return true;
        }
        match event {
            Event::PointerMove(e) => {
                let hovered = self.card_at(e.pos);
                self.set_hovered(cx, hovered);
                self.viewport.contains(e.pos)
            }
            Event::PointerDown(e) if self.viewport.contains(e.pos) => {
                let Some(id) = self.card_at(e.pos) else { return true };
                match e.button {
                    Some(MouseButton::Left) => {
                        self.pressed = Some(id);
                        cx.request_paint();
                    }
                    Some(MouseButton::Right) => {
                        self.open_menu(cx.gfx(), id, e.pos, env, None);
                        cx.hide_tooltip();
                        cx.request_paint();
                    }
                    _ => {}
                }
                true
            }
            Event::PointerUp(e) if self.pressed.is_some() => {
                let pressed = self.pressed.take();
                if e.button == Some(MouseButton::Left) && pressed.is_some() && self.card_at(e.pos) == pressed {
                    let plain = env.options.plain_by_default != e.mods.shift;
                    if let Some(id) = pressed {
                        self.run(cx, id, Command::Paste { plain }, env, out);
                    }
                }
                cx.request_paint();
                true
            }
            Event::PointerLeave | Event::PointerCancel => {
                self.pressed = None;
                self.set_hovered(cx, None);
                false
            }
            _ => false,
        }
    }

    pub fn paint(&mut self, cx: &mut Ctx, p: &mut Painter, env: &Env, out: &mut Vec<PanelAction>) {
        self.ensure_layout(cx.gfx(), env);
        self.cards.retain(|c| !c.leaving || c.height.is_animating() || c.opacity.is_animating());
        let theme = p.theme().clone();
        let visible = self.visible_ids();
        self.request_thumbnails(out);
        let mask = style::scroll_mask(self.viewport, self.scroll.offset(), self.scroll.max_offset(), self.headers[0].is_some());
        let (view_top, view_bottom) = self.scroll.visible_range();
        let width = self.width;
        let badges: Vec<ClipId> = if env.ctrl_held { visible.iter().take(9).copied().collect() } else { Vec::new() };
        let banner = self.banner;
        let ring = (self.ring.get(), self.ring_opacity.get());
        let pressed = self.pressed;
        let cards = &self.cards;
        let scroll = &self.scroll;
        p.masked(mask, |p| {
            scroll.paint_content(p, |p| {
                if let Some(banner) = banner {
                    paint_banner(p, banner);
                }
                for card in cards.iter() {
                    let rect = card.rect(width);
                    if rect.bottom() < view_top || rect.y > view_bottom || rect.h <= 0.5 {
                        continue;
                    }
                    let badge = badges.iter().position(|id| *id == card.id());
                    paint_card(p, card, rect, env, pressed == Some(card.id()), badge);
                }
                if ring.1 > 0.0 {
                    let r = p.snap_rect(ring.0).inset(1.0);
                    p.stroke_round_rect(r, CARD_RADIUS - 1.0, theme.accent.multiply_alpha(ring.1), 2.0);
                }
            });
        });
        if self.place_banner_resume(cx.gfx()) {
            let (viewport, resume) = (self.viewport, &mut self.banner_resume);
            p.clip_rect(viewport, |p| resume.paint(p));
        }
        self.paint_headers(p);
        if visible.is_empty() {
            self.paint_empty(p, env);
        }
        if self.place_hover_buttons(env).is_some() {
            let plate = RectF::new(self.pin.rect().x - PLATE_PAD, self.pin.rect().y - PLATE_PAD, 2.0 * HOVER_BUTTON + 2.0 * PLATE_PAD, HOVER_BUTTON + 2.0 * PLATE_PAD);
            let amount = self.hovered.and_then(|id| self.card(id)).map_or(0.0, |c| c.hover.get());
            let (pin, delete) = (&mut self.pin, &mut self.delete);
            p.clip_rect(self.viewport, |p| {
                p.layer(amount, |p| {
                    style::paint_chip_plate(p, plate, 8.0);
                    pin.paint(p);
                    delete.paint(p);
                });
            });
        }
        self.scroll.paint_scrollbar(p);
    }

    fn request_thumbnails(&mut self, out: &mut Vec<PanelAction>) {
        let (top, bottom) = self.scroll.visible_range();
        let reach = self.viewport.h;
        let width = self.width;
        let wanted: Vec<ClipId> = self
            .cards
            .iter()
            .filter(|c| matches!(c.row.item.content, ClipContent::Image { .. }) && c.row.thumbnail.is_none() && !c.leaving)
            .filter(|c| {
                let r = c.target_rect(width);
                r.bottom() >= top - reach && r.y <= bottom + reach
            })
            .map(Card::id)
            .filter(|id| !self.requested.contains(id))
            .collect();
        if !wanted.is_empty() {
            self.requested.extend(wanted.iter().copied());
            out.push(PanelAction::NeedThumbnails(wanted));
        }
    }

    fn paint_headers(&self, p: &mut Painter) {
        let theme = p.theme().clone();
        let offset = self.scroll.offset();
        let style = style::caption_style();
        p.clip_rect(self.viewport, |p| {
            for (i, (header, title)) in self.headers.iter().zip(["Pinned", "Recent"]).enumerate() {
                let Some(y) = *header else { continue };
                let next = self.headers.get(i + 1).copied().flatten();
                let mut sticky = y.max(offset);
                if let Some(next) = next {
                    sticky = sticky.min(next - SECTION_HEADER);
                }
                let top = self.viewport.y + sticky - offset;
                let rect = RectF::new(PADDING + 4.0, top + 6.0, self.width - 2.0 * PADDING, SECTION_HEADER - 10.0);
                p.text(title, &style, theme.text_secondary, rect);
            }
        });
    }

    fn paint_empty(&self, p: &mut Painter, env: &Env) {
        let theme = p.theme().clone();
        let banner = self.banner.map_or(0.0, |b| b.bottom() + CARD_GAP);
        let area = RectF::new(self.viewport.x, self.viewport.y + banner, self.viewport.w, self.viewport.h - banner);
        let center = PointF::new(area.center().x, area.center().y - 16.0);
        let (icon, title, detail) = if env.query.is_empty() {
            (Icon::Clipboard, "Nothing copied yet".to_string(), "Copy something and it will show up here.".to_string())
        } else {
            (Icon::SearchX, format!("No results for \u{201C}{}\u{201D}", env.query), "Try a different word.".to_string())
        };
        p.fill_circle(p.snap_point(PointF::new(center.x, center.y - 36.0)), 24.0, theme.hover);
        p.icon(icon, PointF::new(center.x, center.y - 36.0), 22.0, theme.text_secondary);
        let title_style = TextStyle::title().centered();
        p.text(&title, &title_style, theme.text, RectF::new(area.x + 24.0, center.y, area.w - 48.0, 20.0));
        let detail_style = TextStyle::body().centered();
        p.text(&detail, &detail_style, theme.text_secondary, RectF::new(area.x + 24.0, center.y + 24.0, area.w - 48.0, 18.0));
    }

    pub fn paint_bar(&mut self, cx: &mut Ctx, p: &mut Painter, env: &Env) {
        self.layout_bar(cx.gfx());
        let theme = p.theme().clone();
        let bar = self.bar;
        let label = style::bar_label_style();
        let text_rect = RectF::new(bar.x + PADDING, bar.y, bar.w / 2.0, bar.h);
        if self.confirming_clear {
            let question = TextStyle::new(12.0).weight(Weight::Semibold);
            p.text("Clear unpinned items?", &question, theme.text, RectF::from_ltrb(text_rect.x, bar.y, self.cancel.rect().x - 8.0, bar.bottom()));
            self.cancel.paint(p);
            self.confirm.paint(p);
            return;
        }
        if env.options.paused {
            p.icon(Icon::PauseFill, PointF::new(text_rect.x + 6.0, bar.center().y), 14.0, theme.text_secondary);
            p.text("Paused", &label, theme.text_secondary, text_rect.offset(18.0, 0.0));
            self.resume.paint(p);
        } else {
            p.text(&format::item_count(self.total), &label.tabular(), theme.text_secondary, text_rect);
        }
        if self.has_unpinned() {
            self.clear.paint(p);
        }
    }

    pub fn paint_overlays(&mut self, p: &mut Painter) {
        if let Some(menu) = &mut self.menu {
            menu.menu.paint(p, None);
            if !menu.menu.is_visible() {
                self.menu = None;
            }
        }
    }

}


fn card_fill(p: &Painter, hover: f32, pressed: bool) -> Color {
    let theme = p.theme();
    let (base, raised) = if theme.is_dark() {
        (Color::rgba(1.0, 1.0, 1.0, 0.065), Color::rgba(1.0, 1.0, 1.0, 0.11))
    } else {
        (Color::rgba(1.0, 1.0, 1.0, 0.55), Color::rgba(1.0, 1.0, 1.0, 0.85))
    };
    let fill = base.lerp(&raised, hover);
    if pressed { fill.lerp(&theme.pressed.over(&fill), 0.6) } else { fill }
}

fn paint_banner(p: &mut Painter, rect: RectF) {
    let theme = p.theme().clone();
    p.fill_round_rect(rect, CARD_RADIUS, theme.accent.with_alpha(if theme.is_dark() { 0.16 } else { 0.10 }));
    p.icon(Icon::PauseFill, PointF::new(rect.x + 20.0, rect.center().y), 16.0, theme.accent);
    let style = TextStyle::body().weight(Weight::Semibold);
    p.text("History is paused", &style, theme.text, RectF::new(rect.x + 36.0, rect.y, rect.w - 120.0, rect.h));
}

fn paint_card(p: &mut Painter, card: &Card, rect: RectF, env: &Env, pressed: bool, badge: Option<usize>) {
    let theme = p.theme().clone();
    let opacity = card.opacity.get().clamp(0.0, 1.0);
    let collapsing = rect.h + 0.5 < card.full_height();
    let rect = p.snap_rect(rect);
    p.layer(opacity, |p| {
        let fill = card_fill(p, card.hover.get(), pressed);
        p.fill_round_rect(rect, CARD_RADIUS, fill);
        if !theme.is_dark() {
            p.hairline_round_rect(rect, CARD_RADIUS, Color::rgba(0.0, 0.0, 0.0, 0.05), true);
        }
        let body = |p: &mut Painter| {
            let inner = RectF::new(rect.x + PAD_X, rect.y + PAD_Y, rect.w - 2.0 * PAD_X, card.body.height());
            paint_body(p, card, inner);
            let meta = RectF::new(rect.x + PAD_X, rect.y + PAD_Y + card.body.height() + META_GAP, rect.w - 2.0 * PAD_X, META);
            paint_meta(p, card, meta, env, badge);
        };
        if collapsing {
            p.clip_round_rect(rect, CARD_RADIUS, body);
        } else {
            body(p);
        }
    });
}

fn paint_lead_icon(p: &mut Painter, icon: Icon, x: f32, line_top: f32, color: Color) {
    p.icon(icon, PointF::new(x + LEAD_ICON / 2.0, line_top + LINE / 2.0), LEAD_ICON, color);
}

fn paint_body(p: &mut Painter, card: &Card, area: RectF) {
    let theme = p.theme().clone();
    let line = |y: f32| RectF::new(area.x + LEAD_WIDTH, y, (area.w - LEAD_WIDTH).max(0.0), LINE);
    match &card.body {
        Body::Text { text, mono, .. } => {
            let style = if *mono { mono_style() } else { text_style() };
            if let Some(layout) = p.layout(text, &style, Some(area.w)) {
                p.draw_layout(&layout, PointF::new(area.x, area.y), theme.text);
            }
        }
        Body::Link { host, url } => {
            paint_lead_icon(p, Icon::Link, area.x, area.y, theme.accent);
            let text_x = area.x + LEAD_WIDTH;
            let text_w = area.w - LEAD_WIDTH;
            p.text(host, &TextStyle::body().weight(Weight::Semibold), theme.text, RectF::new(text_x, area.y, text_w, LINE));
            p.text(url, &TextStyle::new(12.0), theme.text_secondary, RectF::new(text_x, area.y + LINE, text_w, LINE));
        }
        Body::Line { icon, text } => {
            paint_lead_icon(p, *icon, area.x, area.y, theme.text_secondary);
            p.text(text, &TextStyle::body(), theme.text, line(area.y));
        }
        Body::Swatch { color, value } => {
            let swatch = p.snap_rect(RectF::new(area.x, area.y, SWATCH, SWATCH));
            if color.a < 1.0 {
                p.clip_round_rect(swatch, 5.0, |p| p.checkerboard(swatch, 5.0, Color::WHITE, Color::rgb8(204, 204, 204)));
            }
            p.fill_round_rect(swatch, 5.0, *color);
            let edge = if theme.is_dark() { Color::rgba(1.0, 1.0, 1.0, 0.18) } else { Color::rgba(0.0, 0.0, 0.0, 0.14) };
            p.hairline_round_rect(swatch, 5.0, edge, true);
            let text = RectF::new(area.x + SWATCH + 10.0, area.y, area.w - SWATCH - 10.0, SWATCH);
            p.text(value, &TextStyle::body().weight(Weight::Medium), theme.text, text);
        }
        Body::Image { fit, .. } => {
            let frame = p.snap_rect(RectF::new(area.x, area.y, fit.w, fit.h));
            match &card.row.thumbnail {
                Some(bitmap) => {
                    let (light, dark) = if theme.is_dark() {
                        (Color::rgb8(72, 72, 76), Color::rgb8(56, 56, 60))
                    } else {
                        (Color::WHITE, Color::rgb8(226, 226, 230))
                    };
                    p.clip_round_rect(frame, IMAGE_RADIUS, |p| {
                        p.checkerboard(frame, 6.0, light, dark);
                        p.bitmap(bitmap, frame, None, 1.0, Interpolation::Cubic);
                    });
                }
                None => {
                    p.fill_round_rect(frame, IMAGE_RADIUS, theme.hover);
                    p.icon(Icon::Image, frame.center(), 20.0, theme.text_tertiary);
                }
            }
            p.hairline_round_rect(frame, IMAGE_RADIUS, theme.hairline, true);
        }
        Body::Files { icon, names, more } => {
            paint_lead_icon(p, *icon, area.x, area.y, theme.text_secondary);
            let style = TextStyle::body();
            for (i, name) in names.iter().enumerate() {
                p.text(name, &style, theme.text, line(area.y + i as f32 * LINE));
            }
            if *more > 0 {
                let y = area.y + names.len() as f32 * LINE;
                p.text(&format!("+{more} more"), &style, theme.text_secondary, line(y));
            }
        }
    }
}

fn paint_meta(p: &mut Painter, card: &Card, area: RectF, env: &Env, badge: Option<usize>) {
    let theme = p.theme().clone();
    let style = meta_style();
    let color = theme.text_tertiary.lerp(&theme.text_secondary, 0.35);
    let mut x = area.x;
    let icon_rect = p.snap_rect(RectF::new(x, area.center().y - APP_ICON / 2.0, APP_ICON, APP_ICON));
    match &card.row.app_icon {
        Some(icon) => p.bitmap(icon, icon_rect, None, 1.0, Interpolation::Cubic),
        None => p.icon(Icon::AppWindow, icon_rect.center(), APP_ICON, color),
    }
    x += APP_ICON + 6.0;
    let mut right = area.right();
    if let Some(index) = badge {
        right = style::paint_keycap(p, &format!("\u{2303}{}", index + 1), right, area.center().y) - 8.0;
    }
    if let Body::Image { pixels, .. } = card.body {
        let dims = format::dimensions(pixels.0, pixels.1);
        let w = p.measure(&dims, &style.tabular()).w;
        p.text(&dims, &style.tabular().align(TextAlign::Trailing), color, RectF::from_ltrb(area.x, area.y, right, area.bottom()));
        right -= w + 8.0;
    }
    if card.pinned() {
        p.icon(Icon::Pin, PointF::new(right - 6.0, area.center().y), 12.0, color);
        right -= 18.0;
    }
    let app = card.row.app_name.as_deref().unwrap_or("Unknown app");
    let time = format::relative_time(card.row.item.used_ms, env.clock);
    let text = format!("{app} \u{00B7} {time}");
    p.text(&text, &style, color, RectF::from_ltrb(x, area.y, right.max(x), area.bottom()));
}


