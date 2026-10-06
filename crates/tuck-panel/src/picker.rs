//! Emoji, Kaomoji and Symbols tabs: sectioned grids with sticky headers, a selection ring that springs between
//! cells, name labels, the skin-tone popovers and the bottom bar (category buttons with a sliding pill, or
//! scrolling group chips).

use std::time::Duration;

use tuck_core::SkinTone;
use tuck_ui::widgets::{ColorSwatch, Popover, PopoverPlacement, ScrollView, Tooltip};
use tuck_ui::{
    Animated, Brush, Color, Ctx, Event, FontFamily, Gfx, Icon, Key, KeyEvent, MouseButton, Painter, PointF, RectF, SizeF,
    TextStyle, Weight,
};

use crate::panel::Env;
use crate::style::{self, LABEL_TIMER, LONG_PRESS_TIMER, PADDING, SECTION_HEADER};
use crate::{PanelAction, PickerItem, PickerSection, Tab};

const EMOJI_CELL: f32 = 40.0;
pub(crate) const EMOJI_SIZE: f32 = 28.0;
const EMOJI_COLUMNS: usize = 9;
const SYMBOL_CELL: f32 = 36.0;
const SYMBOL_COLUMNS: usize = 10;
const SYMBOL_SIZE: f32 = 20.0;
const KAOMOJI_HEIGHT: f32 = 36.0;
const KAOMOJI_GAP: f32 = 4.0;
const CELL_INSET: f32 = 2.0;
const CELL_RADIUS: f32 = 8.0;
const RING_WIDTH: f32 = 1.5;
const SECTION_GAP: f32 = 8.0;
const FREQUENT_ROWS: usize = 2;
const LABEL_DELAY: Duration = Duration::from_millis(350);
const LONG_PRESS: Duration = Duration::from_millis(500);
const BAR_BUTTON: f32 = 28.0;
const BAR_ICON: f32 = 16.0;
const CHIP_HEIGHT: f32 = 24.0;
const CHIP_PAD: f32 = 10.0;
const CHIP_GAP: f32 = 6.0;
const TONE_CELL: f32 = 40.0;
const SWATCH_GAP: f32 = 4.0;
const HEADER_X: f32 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PickerKind {
    Emoji,
    Kaomoji,
    Symbols,
}

impl PickerKind {
    pub fn tab(self) -> Tab {
        match self {
            PickerKind::Emoji => Tab::Emoji,
            PickerKind::Kaomoji => Tab::Kaomoji,
            PickerKind::Symbols => Tab::Symbols,
        }
    }

    fn uses_chips(self) -> bool {
        self != PickerKind::Emoji
    }
}

fn kaomoji_style() -> TextStyle {
    TextStyle::body().centered()
}

fn symbol_style() -> TextStyle {
    TextStyle::new(SYMBOL_SIZE).family(FontFamily::Symbol).centered()
}

fn chip_style() -> TextStyle {
    TextStyle::new(12.0).weight(Weight::Medium)
}

#[derive(Clone, Copy, Debug)]
struct Cell {
    rect: RectF,
    section: usize,
    item: usize,
}

#[derive(Clone, Copy, Debug)]
struct SectionLayout {
    header: Option<f32>,
    top: f32,
}

struct TonePopover {
    cell: usize,
    popover: Popover,
    highlighted: Option<usize>,
}

struct SkinPopover {
    popover: Popover,
    swatches: Vec<ColorSwatch>,
}

pub(crate) struct PickerTab {
    kind: PickerKind,
    sections: Vec<PickerSection>,
    categories: Vec<(String, Icon)>,
    searching: bool,
    cells: Vec<Cell>,
    rows: Vec<(usize, usize)>,
    layout: Vec<SectionLayout>,
    width: f32,
    dirty: bool,
    scroll: ScrollView,
    viewport: RectF,
    bar: RectF,
    selected: Option<usize>,
    ring: Animated<RectF>,
    ring_opacity: Animated<f32>,
    hovered: Option<usize>,
    hover: Animated<f32>,
    pressed: Option<usize>,
    long_pressed: bool,
    label: Option<usize>,
    label_opacity: Animated<f32>,
    tones: Option<TonePopover>,
    skin: Option<SkinPopover>,
    bar_items: Vec<RectF>,
    bar_hovered: Option<usize>,
    tone_button: RectF,
    pill: Animated<RectF>,
    pill_placed: bool,
    current_section: usize,
    jumping_to: Option<usize>,
    chips_offset: Animated<f32>,
    chips_target: f32,
    chips_width: f32,
}

impl PickerTab {
    pub fn new(kind: PickerKind, scroll_timer: u64) -> Self {
        Self {
            kind,
            sections: Vec::new(),
            categories: Vec::new(),
            searching: false,
            cells: Vec::new(),
            rows: Vec::new(),
            layout: Vec::new(),
            width: 0.0,
            dirty: true,
            scroll: ScrollView::new(scroll_timer),
            viewport: RectF::default(),
            bar: RectF::default(),
            selected: None,
            ring: Animated::new(RectF::default()),
            ring_opacity: Animated::fade(0.0),
            hovered: None,
            hover: Animated::snappy(0.0),
            pressed: None,
            long_pressed: false,
            label: None,
            label_opacity: Animated::fade(0.0),
            tones: None,
            skin: None,
            bar_items: Vec::new(),
            bar_hovered: None,
            tone_button: RectF::default(),
            pill: Animated::new(RectF::default()),
            pill_placed: false,
            current_section: 0,
            jumping_to: None,
            chips_offset: Animated::new(0.0),
            chips_target: 0.0,
            chips_width: 0.0,
        }
    }

    /// New sections; `searching` = they are search results (one flat grid). Selection and scroll restart when the
    /// result set changes kind or `reset` asks for it.
    pub fn set_sections(&mut self, sections: Vec<PickerSection>, searching: bool, reset: bool) {
        if !searching {
            self.categories = sections.iter().filter(|s| !s.items.is_empty()).map(|s| (s.title.clone(), s.icon)).collect();
        }
        let restart = reset || searching != self.searching || self.sections.is_empty();
        self.searching = searching;
        self.sections = sections;
        self.dirty = true;
        self.tones = None;
        self.label = None;
        self.hovered = None;
        if restart {
            self.scroll.snap_to(0.0);
            self.selected = None;
            self.current_section = 0;
            self.jumping_to = None;
        }
    }

    pub fn reset(&mut self) {
        self.scroll.snap_to(0.0);
        self.selected = None;
        self.hovered = None;
        self.pressed = None;
        self.label = None;
        self.label_opacity.snap(0.0);
        self.tones = None;
        self.skin = None;
        self.current_section = 0;
        self.jumping_to = None;
        self.pill_placed = false;
        self.chips_target = 0.0;
        self.chips_offset.snap(0.0);
        self.dirty = true;
    }

    pub fn set_frame(&mut self, viewport: RectF, bar: RectF) {
        if viewport != self.viewport {
            self.dirty = true;
        }
        self.viewport = viewport;
        self.bar = bar;
        self.scroll.set_viewport(viewport);
    }

    pub fn has_overlay(&self) -> bool {
        self.tones.as_ref().is_some_and(|t| t.popover.is_open()) || self.skin.as_ref().is_some_and(|s| s.popover.is_open())
    }

    pub fn dismiss_overlay(&mut self) -> bool {
        if let Some(tones) = self.tones.as_mut().filter(|t| t.popover.is_open()) {
            tones.popover.close();
            return true;
        }
        if let Some(skin) = self.skin.as_mut().filter(|s| s.popover.is_open()) {
            skin.popover.close();
            return true;
        }
        false
    }

    fn item(&self, cell: usize) -> Option<&PickerItem> {
        let cell = self.cells.get(cell)?;
        self.sections.get(cell.section)?.items.get(cell.item)
    }

    fn is_frequent(&self, section: usize) -> bool {
        self.kind == PickerKind::Emoji && !self.searching && section == 0 && self.sections.len() > 1
            && self.sections[0].icon == Icon::Clock
    }

    fn ensure_layout(&mut self, gfx: &Gfx) {
        if !self.dirty && (self.viewport.w - self.width).abs() < 0.01 {
            return;
        }
        self.dirty = false;
        self.width = self.viewport.w;
        self.cells.clear();
        self.rows.clear();
        self.layout.clear();
        let mut y = 0.0;
        for section_index in 0..self.sections.len() {
            let section = &self.sections[section_index];
            let count = section.items.len();
            let header = (!section.title.is_empty() && count > 0).then_some(y);
            if header.is_some() {
                y += SECTION_HEADER;
            }
            self.layout.push(SectionLayout { header, top: header.unwrap_or(y) });
            if count == 0 {
                continue;
            }
            let limit = if self.is_frequent(section_index) { FREQUENT_ROWS * EMOJI_COLUMNS } else { usize::MAX };
            let items: Vec<usize> = (0..count.min(limit)).collect();
            y = match self.kind {
                PickerKind::Emoji => self.grid(section_index, &items, y, EMOJI_CELL, EMOJI_COLUMNS),
                PickerKind::Symbols => self.grid(section_index, &items, y, SYMBOL_CELL, SYMBOL_COLUMNS),
                PickerKind::Kaomoji => self.kaomoji_rows(gfx, section_index, y),
            };
            y += SECTION_GAP;
        }
        self.scroll.set_content_height(y);
        if self.selected.is_none_or(|s| s >= self.cells.len()) {
            self.selected = (!self.cells.is_empty()).then_some(0);
        }
        self.sync_ring(false);
    }

    fn grid(&mut self, section: usize, items: &[usize], top: f32, cell: f32, columns: usize) -> f32 {
        let left = ((self.width - cell * columns as f32) / 2.0).floor();
        let mut y = top;
        for chunk in items.chunks(columns) {
            let start = self.cells.len();
            for (col, &item) in chunk.iter().enumerate() {
                self.cells.push(Cell { rect: RectF::new(left + col as f32 * cell, y, cell, cell), section, item });
            }
            self.rows.push((start, self.cells.len()));
            y += cell;
        }
        y
    }

    /// Rows of three short kaomoji or two longer ones.
    fn kaomoji_rows(&mut self, gfx: &Gfx, section: usize, top: f32) -> f32 {
        let inner = self.width - 2.0 * PADDING;
        let three = ((inner - 2.0 * KAOMOJI_GAP) / 3.0).floor();
        let two = ((inner - KAOMOJI_GAP) / 2.0).floor();
        let fits = |text: &str, width: f32| gfx.measure_text(text, &kaomoji_style()).w + 2.0 * CHIP_PAD <= width;
        let items = &self.sections[section].items;
        let mut y = top;
        let mut index = 0;
        while index < items.len() {
            let take_three = index + 3 <= items.len() && items[index..index + 3].iter().all(|i| fits(&i.text, three));
            let (count, width) = if take_three { (3, three) } else { (2.min(items.len() - index), two) };
            let start = self.cells.len();
            for i in 0..count {
                let rect = RectF::new(PADDING + i as f32 * (width + KAOMOJI_GAP), y, width, KAOMOJI_HEIGHT);
                self.cells.push(Cell { rect, section, item: index + i });
            }
            self.rows.push((start, self.cells.len()));
            index += count;
            y += KAOMOJI_HEIGHT + KAOMOJI_GAP;
        }
        y - KAOMOJI_GAP
    }

    fn highlight_rect(&self, cell: &Cell) -> RectF {
        if self.kind == PickerKind::Kaomoji { cell.rect } else { cell.rect.inset(CELL_INSET) }
    }

    fn sync_ring(&mut self, animate: bool) {
        match self.selected.and_then(|s| self.cells.get(s)).map(|c| self.highlight_rect(c)) {
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

    fn row_of(&self, cell: usize) -> usize {
        self.rows.iter().position(|(start, end)| (*start..*end).contains(&cell)).unwrap_or(0)
    }

    /// The cell in `row` whose center is nearest to `x`.
    fn nearest_in_row(&self, row: usize, x: f32) -> usize {
        let (start, end) = self.rows[row];
        (start..end)
            .min_by(|a, b| {
                let da = (self.cells[*a].rect.center().x - x).abs();
                let db = (self.cells[*b].rect.center().x - x).abs();
                da.total_cmp(&db)
            })
            .unwrap_or(start)
    }

    fn select(&mut self, cx: &mut Ctx, cell: usize, show_label: bool) {
        self.selected = Some(cell);
        self.sync_ring(true);
        let rect = self.cells[cell].rect;
        let header = if self.layout.iter().any(|s| s.header.is_some()) { SECTION_HEADER } else { 0.0 };
        let top = if self.row_of(cell) == 0 { 0.0 } else { rect.y };
        self.scroll.reveal(cx, top, rect.bottom(), 4.0 + header);
        if show_label {
            self.label = Some(cell);
            self.label_opacity.set(1.0);
        }
        cx.request_paint();
    }

    fn insert(&self, cell: usize, close: bool, env: &Env, out: &mut Vec<PanelAction>) {
        if let Some(item) = self.item(cell) {
            out.push(PanelAction::Insert { tab: self.kind.tab(), text: item.toned(env.options.skin_tone).to_string(), close });
        }
    }

    pub fn key(&mut self, cx: &mut Ctx, key: &KeyEvent, env: &Env, out: &mut Vec<PanelAction>) -> bool {
        self.ensure_layout(cx.gfx());
        if self.tones.as_ref().is_some_and(|t| t.popover.is_open()) {
            return self.tones_key(cx, key, out);
        }
        if let Some(skin) = self.skin.as_mut().filter(|s| s.popover.is_open()) {
            if skin.popover.event(cx, &Event::KeyDown(key.clone())).consumed() {
                cx.request_paint();
            }
            return true;
        }
        if self.cells.is_empty() {
            return false;
        }
        let current = self.selected.unwrap_or(0);
        let last = self.cells.len() - 1;
        let page_rows = ((self.viewport.h / self.cells[0].rect.h).floor() as usize).max(1);
        let row = self.row_of(current);
        let x = self.cells[current].rect.center().x;
        let next = match key.key {
            Key::Left => current.saturating_sub(1),
            Key::Right => (current + 1).min(last),
            Key::Up if row > 0 => self.nearest_in_row(row - 1, x),
            Key::Up => current,
            Key::Down if row + 1 < self.rows.len() => self.nearest_in_row(row + 1, x),
            Key::Down => current,
            Key::PageUp => self.nearest_in_row(row.saturating_sub(page_rows), x),
            Key::PageDown => self.nearest_in_row((row + page_rows).min(self.rows.len() - 1), x),
            Key::Home => 0,
            Key::End => last,
            Key::Enter => {
                self.insert(current, true, env, out);
                return true;
            }
            _ => return false,
        };
        self.select(cx, next, true);
        true
    }

    fn tones_key(&mut self, cx: &mut Ctx, key: &KeyEvent, out: &mut Vec<PanelAction>) -> bool {
        let Some(tones) = self.tones.as_mut() else { return false };
        let count = SkinTone::ALL.len();
        let current = tones.highlighted.unwrap_or(0);
        match key.key {
            Key::Left => tones.highlighted = Some(current.saturating_sub(1)),
            Key::Right => tones.highlighted = Some((current + 1).min(count - 1)),
            Key::Enter => {
                let cell = tones.cell;
                tones.popover.close();
                if let Some(text) = self.item(cell).and_then(|i| i.variants.get(current)).cloned() {
                    out.push(PanelAction::Insert { tab: self.kind.tab(), text, close: true });
                }
            }
            Key::Escape => tones.popover.close(),
            _ => {}
        }
        cx.request_paint();
        true
    }

    /// Opens the tone popover for `cell` (emoji with variants only). `highlighted` is for previews.
    pub fn open_tones(&mut self, cell: usize, env: &Env, highlighted: Option<usize>, instant: bool) -> bool {
        let Some(item) = self.item(cell) else { return false };
        if !item.has_tones() {
            return false;
        }
        let mut popover = Popover::new(SizeF::new(TONE_CELL * SkinTone::ALL.len() as f32, TONE_CELL)).opaque();
        popover.placement = PopoverPlacement::Above;
        let anchor = self.scroll.to_window(self.cells[cell].rect);
        if instant {
            popover.force_open(anchor, env.size);
        } else {
            popover.open(anchor, env.size);
        }
        let current = SkinTone::ALL.iter().position(|t| *t == env.options.skin_tone);
        self.tones = Some(TonePopover { cell, popover, highlighted: highlighted.or(current) });
        self.label = None;
        true
    }

    fn open_skin_picker(&mut self, env: &Env, instant: bool) {
        let count = SkinTone::ALL.len() as f32;
        let mut popover =
            Popover::new(SizeF::new(count * ColorSwatch::SIZE + (count - 1.0) * SWATCH_GAP, ColorSwatch::SIZE)).opaque();
        popover.placement = PopoverPlacement::Above;
        if instant {
            popover.force_open(self.tone_button, env.size);
        } else {
            popover.open(self.tone_button, env.size);
        }
        let content = popover.content_rect();
        let swatches = SkinTone::ALL
            .iter()
            .enumerate()
            .map(|(i, tone)| {
                let mut swatch = ColorSwatch::new(style::tone_color(*tone))
                    .tooltip(style::tone_name(*tone))
                    .with_selected(*tone == env.options.skin_tone);
                let x = content.x + ColorSwatch::SIZE / 2.0 + i as f32 * (ColorSwatch::SIZE + SWATCH_GAP);
                swatch.set_center(PointF::new(x, content.center().y));
                swatch
            })
            .collect();
        self.skin = Some(SkinPopover { popover, swatches });
    }

    /// Previews: open the skin-tone swatch popover.
    pub fn force_skin_picker(&mut self, gfx: &Gfx, env: &Env) {
        self.ensure_layout(gfx);
        self.layout_bar(gfx);
        self.open_skin_picker(env, true);
    }

    /// Previews: hover `cell` with its name label showing (or `tones` open on it).
    pub fn force_hover(&mut self, gfx: &Gfx, cell: usize, label: bool) {
        self.ensure_layout(gfx);
        self.hovered = Some(cell);
        self.hover.snap(1.0);
        if label {
            self.label = Some(cell);
            self.label_opacity.snap(1.0);
        }
    }

    pub fn force_select(&mut self, gfx: &Gfx, cell: usize) {
        self.ensure_layout(gfx);
        self.selected = Some(cell.min(self.cells.len().saturating_sub(1)));
        self.sync_ring(false);
    }

    pub fn force_scroll(&mut self, gfx: &Gfx, offset: f32) {
        self.ensure_layout(gfx);
        self.scroll.snap_to(offset);
        self.scroll.force_scrollbar(1.0);
    }

    /// Index of the first cell showing `text` (previews).
    pub fn cell_of(&mut self, gfx: &Gfx, text: &str) -> Option<usize> {
        self.ensure_layout(gfx);
        (0..self.cells.len()).find(|i| self.item(*i).is_some_and(|item| item.text == text))
    }

    fn cell_at(&self, pos: PointF) -> Option<usize> {
        if !self.viewport.contains(pos) {
            return None;
        }
        let content = self.scroll.to_content(pos);
        self.cells.iter().position(|c| c.rect.contains(content))
    }

    fn set_hovered(&mut self, cx: &mut Ctx, cell: Option<usize>) {
        if self.hovered == cell {
            return;
        }
        self.hovered = cell;
        self.hover.snap(0.0);
        if cell.is_some() {
            self.hover.set(1.0);
            cx.set_timer(LABEL_DELAY, LABEL_TIMER);
        }
        if self.label.is_some() && self.label != cell {
            self.label_opacity.set(0.0);
        }
        cx.request_paint();
    }

    fn layout_bar(&mut self, gfx: &Gfx) {
        let bar = self.bar;
        let center = bar.center().y;
        self.bar_items.clear();
        if self.kind.uses_chips() {
            let mut x = 0.0;
            for (title, _) in &self.categories {
                let w = (gfx.measure_text(title, &chip_style()).w + 2.0 * CHIP_PAD).ceil();
                self.bar_items.push(RectF::new(x, center - CHIP_HEIGHT / 2.0, w, CHIP_HEIGHT));
                x += w + CHIP_GAP;
            }
            self.chips_width = (x - CHIP_GAP).max(0.0);
        } else {
            self.tone_button = RectF::new(bar.right() - PADDING - BAR_BUTTON, center - BAR_BUTTON / 2.0, BAR_BUTTON, BAR_BUTTON);
            let count = self.categories.len().max(1) as f32;
            let room = self.tone_button.x - 8.0 - (bar.x + PADDING);
            let gap = ((room - count * BAR_BUTTON) / (count - 1.0).max(1.0)).clamp(0.0, 8.0).floor();
            for i in 0..self.categories.len() {
                let x = bar.x + PADDING + i as f32 * (BAR_BUTTON + gap);
                self.bar_items.push(RectF::new(x, center - BAR_BUTTON / 2.0, BAR_BUTTON, BAR_BUTTON));
            }
        }
    }

    fn chips_area(&self) -> RectF {
        RectF::new(self.bar.x + PADDING, self.bar.y, self.bar.w - 2.0 * PADDING, self.bar.h)
    }

    /// Bar item rect in window coordinates (chips scroll horizontally).
    fn bar_item_rect(&self, index: usize) -> RectF {
        let rect = self.bar_items[index];
        if self.kind.uses_chips() { rect.offset(self.chips_area().x - self.chips_offset.get(), 0.0) } else { rect }
    }

    /// Section index of category `index` (categories skip empty sections).
    fn section_of_category(&self, index: usize) -> Option<usize> {
        let (title, icon) = self.categories.get(index)?;
        self.sections.iter().position(|s| &s.title == title && s.icon == *icon && !s.items.is_empty())
    }

    fn category_of_section(&self, section: usize) -> Option<usize> {
        let s = self.sections.get(section)?;
        self.categories.iter().position(|(title, icon)| *title == s.title && *icon == s.icon)
    }

    fn jump_to_category(&mut self, cx: &mut Ctx, index: usize) {
        let Some(section) = self.section_of_category(index) else { return };
        let top = self.layout.get(section).map_or(0.0, |s| s.top);
        self.jumping_to = Some(index);
        self.current_section = index;
        self.scroll.scroll_to(cx, top, true);
        if let Some(cell) = self.cells.iter().position(|c| c.section == section) {
            self.selected = Some(cell);
            self.sync_ring(false);
        }
        self.reveal_chip(index, true);
        cx.request_paint();
    }

    fn reveal_chip(&mut self, index: usize, animate: bool) {
        if !self.kind.uses_chips() || index >= self.bar_items.len() {
            return;
        }
        let area = self.chips_area();
        let max = (self.chips_width - area.w).max(0.0);
        let chip = self.bar_items[index];
        let target = if chip.x < self.chips_target {
            chip.x - 8.0
        } else if chip.right() > self.chips_target + area.w {
            chip.right() - area.w + 8.0
        } else {
            self.chips_target
        };
        self.chips_target = target.clamp(0.0, max);
        if animate {
            self.chips_offset.set(self.chips_target);
        } else {
            self.chips_offset.snap(self.chips_target);
        }
    }

    /// The category the scroll position is in (or the one a jump is heading to).
    fn update_current_section(&mut self) {
        if let Some(target) = self.jumping_to {
            if self.scroll.offset() == self.scroll.target() {
                self.jumping_to = None;
            }
            self.current_section = target;
            return;
        }
        let offset = self.scroll.offset() + 1.0;
        let at_end = self.scroll.offset() >= self.scroll.max_offset() - 1.0 && self.scroll.can_scroll();
        let section = if at_end {
            self.layout.iter().rposition(|_| true)
        } else {
            self.layout.iter().rposition(|s| s.top <= offset)
        };
        let category = section.and_then(|s| {
            (0..=s).rev().find_map(|i| (!self.sections[i].items.is_empty()).then(|| self.category_of_section(i)).flatten())
        });
        if let Some(category) = category
            && category != self.current_section
        {
            self.current_section = category;
            self.reveal_chip(category, true);
        }
    }

    fn bar_event(&mut self, cx: &mut Ctx, event: &Event, env: &Env) -> bool {
        self.layout_bar(cx.gfx());
        let enabled = !self.searching;
        match event {
            Event::Wheel(w) if self.bar.contains(w.pos) && self.kind.uses_chips() => {
                let area = self.chips_area();
                let delta = if w.delta.x != 0.0 { w.delta.x } else { -w.delta.y };
                self.chips_target = (self.chips_target + delta * 48.0).clamp(0.0, (self.chips_width - area.w).max(0.0));
                if w.precise {
                    self.chips_offset.snap(self.chips_target);
                } else {
                    self.chips_offset.set(self.chips_target);
                }
                cx.request_paint();
                true
            }
            Event::PointerMove(e) => {
                let hovered = (0..self.bar_items.len()).find(|i| enabled && self.bar_item_rect(*i).contains(e.pos));
                let on_tone = !self.kind.uses_chips() && self.tone_button.contains(e.pos);
                let hovered = hovered.or(on_tone.then_some(usize::MAX));
                if hovered != self.bar_hovered {
                    if let Some(old) = self.bar_hovered.filter(|i| *i != usize::MAX) {
                        cx.hide_tooltip_for(self.bar_item_rect(old));
                    }
                    if let Some(index) = hovered {
                        if index == usize::MAX {
                            cx.show_tooltip(self.tone_button, "Skin tone", None);
                        } else if !self.kind.uses_chips() {
                            cx.show_tooltip(self.bar_item_rect(index), &self.categories[index].0, None);
                        }
                    }
                    self.bar_hovered = hovered;
                    cx.request_paint();
                }
                self.bar.contains(e.pos)
            }
            Event::PointerDown(e) if self.bar.contains(e.pos) => {
                if e.button == Some(MouseButton::Left) {
                    if !self.kind.uses_chips() && self.tone_button.contains(e.pos) {
                        cx.hide_tooltip();
                        self.open_skin_picker(env, false);
                    } else if let Some(index) = (0..self.bar_items.len()).find(|i| self.bar_item_rect(*i).contains(e.pos))
                        && enabled
                    {
                        self.jump_to_category(cx, index);
                    }
                    cx.request_paint();
                }
                true
            }
            Event::PointerUp(e) => self.bar.contains(e.pos),
            Event::PointerLeave => {
                if self.bar_hovered.take().is_some() {
                    cx.request_paint();
                }
                false
            }
            _ => false,
        }
    }

    fn popover_event(&mut self, cx: &mut Ctx, event: &Event, env: &Env, out: &mut Vec<PanelAction>) -> Option<bool> {
        if let Some(tones) = self.tones.as_mut().filter(|t| t.popover.is_open()) {
            let content = tones.popover.content_rect();
            let index = |pos: PointF| content.contains(pos).then(|| ((pos.x - content.x) / TONE_CELL) as usize).filter(|i| *i < SkinTone::ALL.len());
            match event {
                Event::PointerMove(e) => {
                    if let Some(i) = index(e.pos) {
                        tones.highlighted = Some(i);
                    }
                }
                Event::PointerUp(e) if e.button == Some(MouseButton::Left) => {
                    if let Some(i) = index(e.pos) {
                        let cell = tones.cell;
                        tones.popover.close();
                        if let Some(text) = self.item(cell).and_then(|item| item.variants.get(i)).cloned() {
                            out.push(PanelAction::Insert { tab: self.kind.tab(), text, close: env.options.close_after_click });
                        }
                    }
                }
                _ => {}
            }
            if let Some(tones) = self.tones.as_mut() {
                tones.popover.event(cx, event);
            }
            cx.request_paint();
            return Some(true);
        }
        if let Some(skin) = self.skin.as_mut().filter(|s| s.popover.is_open()) {
            for (i, swatch) in skin.swatches.iter_mut().enumerate() {
                if swatch.event(cx, event).action().is_some() {
                    let tone = SkinTone::ALL[i];
                    out.push(PanelAction::SkinTone(tone));
                    skin.popover.close();
                    cx.request_paint();
                    return Some(true);
                }
            }
            skin.popover.event(cx, event);
            cx.request_paint();
            return Some(true);
        }
        None
    }

    pub fn event(&mut self, cx: &mut Ctx, event: &Event, env: &Env, out: &mut Vec<PanelAction>) -> bool {
        self.ensure_layout(cx.gfx());
        if let Some(handled) = self.popover_event(cx, event, env, out) {
            return handled;
        }
        if self.bar_event(cx, event, env) {
            return true;
        }
        if self.scroll.event(cx, event).consumed() {
            if let Event::Wheel(w) = event {
                let hovered = self.cell_at(w.pos);
                self.set_hovered(cx, hovered);
            }
            return true;
        }
        match event {
            Event::Timer(LABEL_TIMER) => {
                if let Some(cell) = self.hovered {
                    self.label = Some(cell);
                    self.label_opacity.set(1.0);
                    cx.request_paint();
                }
                true
            }
            Event::Timer(LONG_PRESS_TIMER) => {
                if let Some(cell) = self.pressed
                    && self.open_tones(cell, env, None, false)
                {
                    self.long_pressed = true;
                    cx.request_paint();
                }
                true
            }
            Event::PointerMove(e) => {
                let hovered = self.cell_at(e.pos);
                if self.pressed.is_some() && hovered != self.pressed {
                    self.pressed = None;
                }
                self.set_hovered(cx, hovered);
                self.viewport.contains(e.pos)
            }
            Event::PointerDown(e) if self.viewport.contains(e.pos) => {
                let Some(cell) = self.cell_at(e.pos) else { return true };
                match e.button {
                    Some(MouseButton::Left) => {
                        self.pressed = Some(cell);
                        self.long_pressed = false;
                        if self.item(cell).is_some_and(PickerItem::has_tones) {
                            cx.set_timer(LONG_PRESS, LONG_PRESS_TIMER);
                        }
                    }
                    Some(MouseButton::Right) if self.open_tones(cell, env, None, false) => cx.hide_tooltip(),
                    _ => {}
                }
                cx.request_paint();
                true
            }
            Event::PointerUp(e) if self.pressed.is_some() => {
                let pressed = self.pressed.take();
                if !std::mem::take(&mut self.long_pressed)
                    && e.button == Some(MouseButton::Left)
                    && pressed.is_some()
                    && self.cell_at(e.pos) == pressed
                    && let Some(cell) = pressed
                {
                    self.selected = Some(cell);
                    self.sync_ring(true);
                    self.insert(cell, env.options.close_after_click, env, out);
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

    pub fn paint(&mut self, cx: &mut Ctx, p: &mut Painter, env: &Env) {
        self.ensure_layout(cx.gfx());
        self.layout_bar(cx.gfx());
        self.update_current_section();
        let theme = p.theme().clone();
        let headers = self.layout.iter().any(|s| s.header.is_some());
        let (top, bottom) = self.scroll.visible_range();
        let visible: Vec<usize> = (0..self.cells.len())
            .filter(|i| {
                let r = self.cells[*i].rect;
                r.bottom() >= top && r.y <= bottom
            })
            .collect();
        let tone = env.options.skin_tone;
        if self.kind == PickerKind::Emoji {
            let texts: Vec<&str> = visible.iter().filter_map(|i| self.item(*i)).map(|item| item.toned(tone)).collect();
            p.prepare_emoji(&texts, EMOJI_SIZE);
        }
        let mask = style::scroll_mask(self.viewport, self.scroll.offset(), self.scroll.max_offset(), headers);
        p.masked(mask, |p| {
            self.scroll.paint_content(p, |p| {
                for &i in &visible {
                    self.paint_cell(p, i, tone, &theme);
                }
                let opacity = self.ring_opacity.get();
                if opacity > 0.0 {
                    let ring = p.snap_rect(self.ring.get()).inset(RING_WIDTH / 2.0);
                    p.stroke_round_rect(ring, CELL_RADIUS - RING_WIDTH / 2.0, theme.accent.multiply_alpha(opacity), RING_WIDTH);
                }
            });
        });
        self.paint_headers(p);
        if self.cells.is_empty() {
            self.paint_empty(p, env);
        }
        self.scroll.paint_scrollbar(p);
    }

    fn paint_cell(&self, p: &mut Painter, index: usize, tone: SkinTone, theme: &tuck_ui::Theme) {
        let cell = &self.cells[index];
        let Some(item) = self.item(index) else { return };
        let highlight = self.highlight_rect(cell);
        let selected = self.selected == Some(index);
        let hovered = if self.hovered == Some(index) { self.hover.get() } else { 0.0 };
        let pressed = self.pressed == Some(index);
        let base = if self.kind == PickerKind::Kaomoji { theme.hover.multiply_alpha(0.7) } else { Color::TRANSPARENT };
        let mut fill = base.lerp(&theme.hover, hovered);
        if self.kind == PickerKind::Kaomoji {
            fill = fill.lerp(&theme.pressed, hovered * 0.6);
        }
        if selected {
            fill = theme.selected;
        }
        if pressed {
            fill = theme.pressed.over(&fill);
        }
        if fill.a > 0.0 {
            p.fill_round_rect(p.snap_rect(highlight), CELL_RADIUS, fill);
        }
        match self.kind {
            PickerKind::Emoji => p.emoji(item.toned(tone), cell.rect.center(), EMOJI_SIZE),
            PickerKind::Symbols => {
                p.text(&item.text, &symbol_style(), theme.text, cell.rect);
            }
            PickerKind::Kaomoji => {
                p.text(&item.text, &kaomoji_style(), theme.text, cell.rect.inset(4.0));
            }
        }
    }

    fn paint_headers(&self, p: &mut Painter) {
        let theme = p.theme().clone();
        let offset = self.scroll.offset();
        let style = style::caption_style();
        let headers: Vec<(usize, f32)> = self.layout.iter().enumerate().filter_map(|(i, s)| s.header.map(|y| (i, y))).collect();
        p.clip_rect(self.viewport, |p| {
            for (n, &(section, y)) in headers.iter().enumerate() {
                let next = headers.get(n + 1).map(|h| h.1);
                if next.is_some_and(|next| next <= offset) {
                    continue;
                }
                let mut sticky = y.max(offset);
                if let Some(next) = next {
                    sticky = sticky.min(next - SECTION_HEADER);
                }
                let top = self.viewport.y + sticky - offset;
                if top > self.viewport.bottom() {
                    break;
                }
                let rect = RectF::new(HEADER_X, top + 6.0, self.width - 2.0 * HEADER_X, SECTION_HEADER - 10.0);
                p.text(&self.sections[section].title, &style, theme.text_secondary, rect);
            }
        });
    }

    fn paint_empty(&self, p: &mut Painter, env: &Env) {
        let theme = p.theme().clone();
        let center = PointF::new(self.viewport.center().x, self.viewport.center().y - 16.0);
        let title = if env.query.is_empty() { "Nothing here yet".to_string() } else { format!("No results for \u{201C}{}\u{201D}", env.query) };
        p.fill_circle(p.snap_point(PointF::new(center.x, center.y - 36.0)), 24.0, theme.hover);
        p.icon(Icon::SearchX, PointF::new(center.x, center.y - 36.0), 22.0, theme.text_secondary);
        p.text(&title, &TextStyle::title().centered(), theme.text, RectF::new(self.viewport.x + 24.0, center.y, self.viewport.w - 48.0, 20.0));
        let detail = if env.query.is_empty() { "" } else { "Try a different word." };
        p.text(detail, &TextStyle::body().centered(), theme.text_secondary, RectF::new(self.viewport.x + 24.0, center.y + 24.0, self.viewport.w - 48.0, 18.0));
    }

    pub fn paint_bar(&mut self, cx: &mut Ctx, p: &mut Painter, env: &Env) {
        self.layout_bar(cx.gfx());
        if self.kind.uses_chips() {
            self.paint_chips(p);
        } else {
            self.paint_categories(p, env);
        }
    }

    fn paint_categories(&mut self, p: &mut Painter, env: &Env) {
        let theme = p.theme().clone();
        let enabled = !self.searching;
        if let Some(target) = self.bar_items.get(self.current_section).copied() {
            if self.pill_placed {
                self.pill.set(target);
            } else {
                self.pill.snap(target);
                self.pill_placed = true;
            }
        }
        let pill_opacity = if enabled && !self.bar_items.is_empty() { 1.0 } else { 0.0 };
        if pill_opacity > 0.0 {
            p.fill_round_rect(p.snap_rect(self.pill.get()), CELL_RADIUS, theme.accent.with_alpha(if theme.is_dark() { 0.24 } else { 0.14 }));
        }
        for (i, (_, icon)) in self.categories.iter().enumerate() {
            let rect = self.bar_items[i];
            let current = enabled && i == self.current_section;
            if enabled && self.bar_hovered == Some(i) && !current {
                p.fill_round_rect(p.snap_rect(rect), CELL_RADIUS, theme.hover);
            }
            let color = if !enabled {
                theme.text_tertiary.multiply_alpha(0.6)
            } else if current {
                theme.accent
            } else {
                theme.text_secondary
            };
            p.icon(*icon, rect.center(), BAR_ICON, color);
        }
        let tone = self.tone_button;
        if self.bar_hovered == Some(usize::MAX) || self.skin.as_ref().is_some_and(|s| s.popover.is_open()) {
            p.fill_round_rect(p.snap_rect(tone), CELL_RADIUS, theme.hover);
        }
        let center = p.snap_point(tone.center());
        p.fill_circle(center, 8.0, style::tone_color(env.options.skin_tone));
        let edge = if theme.is_dark() { Color::rgba(1.0, 1.0, 1.0, 0.25) } else { Color::rgba(0.0, 0.0, 0.0, 0.18) };
        p.stroke_ellipse(center, 8.0 - p.px() / 2.0, 8.0 - p.px() / 2.0, edge, p.px());
    }

    fn paint_chips(&mut self, p: &mut Painter) {
        let theme = p.theme().clone();
        let area = self.chips_area();
        let enabled = !self.searching;
        let offset = self.chips_offset.get();
        let overflow = self.chips_width > area.w;
        let left_fade = if overflow && offset > 0.5 { 24.0 } else { 0.0 };
        let right_fade = if overflow && offset < self.chips_width - area.w - 0.5 { 24.0 } else { 0.0 };
        let mask = horizontal_fade(area, left_fade, right_fade);
        let style = chip_style();
        let items: Vec<(RectF, &str)> =
            (0..self.bar_items.len()).map(|i| (self.bar_item_rect(i), self.categories[i].0.as_str())).collect();
        let (current, hovered) = (self.current_section, self.bar_hovered);
        p.clip_rect(area, |p| {
            p.masked(mask, |p| {
                for (i, (rect, title)) in items.iter().enumerate() {
                    let rect = p.snap_rect(*rect);
                    let is_current = enabled && i == current;
                    let (fill, color) = if is_current {
                        (theme.accent.with_alpha(if theme.is_dark() { 0.24 } else { 0.14 }), theme.accent)
                    } else if enabled && hovered == Some(i) {
                        (theme.hover, theme.text)
                    } else {
                        (Color::TRANSPARENT, if enabled { theme.text_secondary } else { theme.text_tertiary })
                    };
                    if fill.a > 0.0 {
                        p.fill_round_rect(rect, CHIP_HEIGHT / 2.0, fill);
                    }
                    p.text(title, &style.centered(), color, rect);
                }
            });
        });
    }

    /// The emoji texts in display order (for prewarming the rasterizer), at most `limit`.
    pub fn display_texts(&self, tone: SkinTone, limit: usize) -> Vec<&str> {
        self.sections.iter().flat_map(|s| &s.items).take(limit).map(|item| item.toned(tone)).collect()
    }

    pub fn paint_overlays(&mut self, p: &mut Painter, env: &Env) {
        let theme = p.theme().clone();
        if let Some(cell) = self.label
            && self.tones.as_ref().is_none_or(|t| !t.popover.is_visible())
        {
            let opacity = self.label_opacity.get();
            if opacity > 0.0 && cell < self.cells.len() {
                let anchor = self.scroll.to_window(self.highlight_rect(&self.cells[cell]));
                if anchor.bottom() > self.viewport.y && anchor.y < self.viewport.bottom() {
                    let name = self.item(cell).map(|i| i.name.clone()).unwrap_or_default();
                    if !name.is_empty() {
                        let fits_above = Tooltip::frame_above(p, anchor, &name, None, env.size).y >= self.viewport.y;
                        if fits_above {
                            Tooltip::paint_bubble_above(p, anchor, &name, opacity, env.size);
                        } else {
                            Tooltip::paint_bubble(p, anchor, &name, None, opacity, env.size);
                        }
                    }
                }
            } else if opacity <= 0.0 {
                self.label = None;
            }
        }
        if let Some(tones) = &mut self.tones {
            let item = self.cells.get(tones.cell).and_then(|c| self.sections.get(c.section)?.items.get(c.item)).cloned();
            let highlighted = tones.highlighted;
            tones.popover.paint(p, None, |p, content| {
                let Some(item) = &item else { return };
                let texts: Vec<&str> = item.variants.iter().map(String::as_str).collect();
                p.prepare_emoji(&texts, EMOJI_SIZE);
                for (i, text) in texts.iter().enumerate() {
                    let cell = RectF::new(content.x + i as f32 * TONE_CELL, content.y, TONE_CELL, TONE_CELL);
                    if highlighted == Some(i) {
                        p.fill_round_rect(p.snap_rect(cell.inset(CELL_INSET)), CELL_RADIUS, theme.hover);
                    }
                    p.emoji(text, cell.center(), EMOJI_SIZE);
                }
            });
            if !tones.popover.is_visible() {
                self.tones = None;
            }
        }
        if let Some(skin) = &mut self.skin {
            let swatches = &mut skin.swatches;
            skin.popover.paint(p, None, |p, _| {
                for swatch in swatches.iter_mut() {
                    swatch.paint(p);
                }
            });
            if !skin.popover.is_visible() {
                self.skin = None;
            }
        }
    }

}

fn horizontal_fade(area: RectF, left: f32, right: f32) -> Brush {
    let w = area.w.max(1.0);
    let clear = Color::rgba(0.0, 0.0, 0.0, 0.0);
    let mut stops = vec![(0.0, if left > 0.0 { clear } else { Color::BLACK })];
    if left > 0.0 {
        stops.push(((left / w).min(0.5), Color::BLACK));
    }
    if right > 0.0 {
        stops.push((1.0 - (right / w).min(0.5), Color::BLACK));
        stops.push((1.0, clear));
    } else {
        stops.push((1.0, Color::BLACK));
    }
    Brush::linear(PointF::new(area.x, area.y), PointF::new(area.right(), area.y), &stops)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(title: &str, icon: Icon, count: usize) -> PickerSection {
        let items = (0..count).map(|i| PickerItem::new(&format!("{i}"), &format!("item {i}"))).collect();
        PickerSection { title: title.into(), icon, items }
    }

    fn laid_out(kind: PickerKind, sections: Vec<PickerSection>) -> PickerTab {
        let gfx = Gfx::new().expect("graphics devices");
        let mut tab = PickerTab::new(kind, 3);
        tab.set_sections(sections, false, true);
        tab.set_frame(RectF::new(0.0, 94.0, 380.0, 326.0), RectF::new(0.0, 420.0, 380.0, 40.0));
        tab.ensure_layout(&gfx);
        tab
    }

    #[test]
    fn emoji_grid_is_nine_columns_with_a_two_row_frequent_section() {
        let tab = laid_out(PickerKind::Emoji, vec![section("Frequently used", Icon::Clock, 30), section("Smileys", Icon::Smile, 20)]);
        assert_eq!(tab.cells.iter().filter(|c| c.section == 0).count(), 18);
        assert_eq!(tab.rows[0], (0, 9));
        assert_eq!(tab.cells[0].rect, RectF::new(10.0, SECTION_HEADER, 40.0, 40.0));
        let second = tab.layout[1].header.unwrap();
        assert_eq!(second, SECTION_HEADER + 80.0 + SECTION_GAP);
    }

    #[test]
    fn vertical_moves_keep_the_column() {
        let tab = laid_out(PickerKind::Emoji, vec![section("A", Icon::Smile, 12), section("B", Icon::Heart, 9)]);
        let row = tab.row_of(11);
        assert_eq!(row, 1);
        assert_eq!(tab.nearest_in_row(2, tab.cells[2].rect.center().x), 14);
        assert_eq!(tab.nearest_in_row(1, tab.cells[8].rect.center().x), 11, "short rows clamp to their last cell");
    }

    #[test]
    fn short_kaomoji_share_rows_of_three() {
        let mut sections = vec![section("Happy", Icon::Smile, 0)];
        sections[0].items = ["(^_^)", "(*^‿^*)", "(◕‿◕)", "ヽ(°〇°)ﾉ looking around everywhere", "(o_o)"]
            .iter()
            .map(|t| PickerItem::new(t, t))
            .collect();
        let tab = laid_out(PickerKind::Kaomoji, sections);
        assert_eq!(tab.rows[0].1 - tab.rows[0].0, 3);
        assert_eq!(tab.rows[1].1 - tab.rows[1].0, 2);
    }
}
