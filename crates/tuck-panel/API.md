# tuck-panel API

The panel (DESIGN §7): one `tuck_ui::View` with Clipboard, Emoji, Kaomoji and Symbols tabs, for a pre-created,
hidden, never-activated `WindowSpec::panel` window of `Panel::SIZE` (380×460 DIP). Data comes in through setters,
results go out as `PanelAction`s posted with `cx.post` (register `app.on_event(|app, a: PanelAction| ..)`). The panel
makes no Windows calls beyond tuck-ui and never depends on sys/clip/store/emoji crates.

## Types
```rust
pub enum Tab { Clipboard, Emoji, Kaomoji, Symbols }        // ALL, index(), from_index(), title(), placeholder(),
                                                            // icon(), step(forward)
pub struct PickerItem { pub text: String, pub name: String, pub variants: Vec<String> /* 6 incl. default, or empty */ }
impl PickerItem { fn new(text, name); fn with_variants(v); fn toned(&self, SkinTone) -> &str; fn has_tones() }
pub struct PickerSection { pub title: String, pub icon: Icon, pub items: Vec<PickerItem> }
pub struct ClipRow { pub item: ClipItem, pub kind: TextKind, pub thumbnail: Option<Rc<Bitmap>>,
                     pub app_icon: Option<Rc<Bitmap>>, pub app_name: Option<String> }
pub enum PanelAction {
    Insert { tab: Tab, text: String, close: bool }, Paste { id: ClipId, plain: bool }, Copy { id: ClipId },
    SetPinned { id: ClipId, pinned: bool }, Delete { id: ClipId }, ClearUnpinned, CopyImageText { id: ClipId },
    EditInGlint { id: ClipId }, ShowInExplorer { id: ClipId }, QueryChanged { tab: Tab, query: String },
    TabChanged(Tab), NeedThumbnails(Vec<ClipId>), SkinTone(SkinTone), SetPaused(bool), OpenSettings,
    DragTo(PointI) /* new window origin, physical px */, Close,
}
pub struct PanelOptions { pub skin_tone: SkinTone, pub close_after_click: bool, pub plain_by_default: bool,
                          pub glint_available: bool, pub paused: bool }   // Copy, Default
pub struct Clock { pub now_ms: i64, pub utc_offset_minutes: i32 }        // fixed "now" for previews
```

## Panel
```rust
impl Panel {
    pub const SIZE: SizeF;                                    // 380×460 DIP
    pub fn new(options: PanelOptions) -> Self;
    pub fn set_options(&mut self, cx: &mut Ctx, options: PanelOptions);
    pub fn options(&self) -> PanelOptions;
    pub fn set_backdrop(&mut self, backdrop: PanelBackdrop);  // Solid → opaque fill; else translucent glass
    pub fn set_clock(&mut self, clock: Option<Clock>);        // None = system clock (default)
    pub fn open(&mut self, cx: &mut Ctx, tab: Tab);           // clears query + selection, top of lists, appear motion
    pub fn close(&mut self, cx: &mut Ctx);                    // 120 ms disappear; then hide when is_closing_done()
    pub fn is_closing_done(&self) -> bool;
    pub fn is_open(&self) -> bool;
    pub fn tab(&self) -> Tab;
    pub fn select_tab(&mut self, cx: &mut Ctx, tab: Tab);      // switch while open, as a segment click would
    pub fn query(&self) -> &str;
    pub fn set_clips(&mut self, cx: &mut Ctx, rows: Vec<ClipRow>, total: usize);   // filtered by query, history order
    pub fn set_thumbnail(&mut self, cx: &mut Ctx, id: ClipId, bitmap: Rc<Bitmap>);
    pub fn set_sections(&mut self, cx: &mut Ctx, tab: Tab, sections: Vec<PickerSection>); // search → 1 section
    pub fn panel_rect(&self) -> RectF;                        // whole window: the window rect is the panel
    pub fn skin_tone(&self) -> SkinTone;
}
```
- Rows: pinned rows form "Pinned" (headers show only when something is pinned), the rest "Recent"; order is kept.
  Rows that disappear collapse with a height spring, new ones fade in, pinned ones slide. `total` feeds "N items".
- Thumbnails: the panel posts `NeedThumbnails` once per id for image cards near the viewport; answer with
  `set_thumbnail` (about 2× the card size; drawn fit into width × 110 DIP over a checkerboard).
- Sections: an empty title draws no header. In Emoji, a first section with `Icon::Clock` is "Frequently used" (2 rows).
  While the query is non-empty sections are search results (selection and scroll restart, category bar dims).
  Emoji `set_sections` also queues background rasterization of the first 400 emoji at the window's scale.
- The panel applies `skin_tone` itself (`PickerItem::toned`); the skin-tone popover posts `SkinTone(t)` and updates
  its own copy (send `set_options` to persist).

## Input (the window never has focus: route keys with `App::with_view`)
Feed `Event::KeyDown(KeyEvent)` (+ `Event::Text(String)` for printable text) and `Event::KeyUp` for `Key::Control`.
- Text edits the search (`QueryChanged`); Backspace / Ctrl+Backspace (word); Ctrl+A selects the query; Shift+Left/Right
  move the caret when the query is non-empty, otherwise arrows move the selection.
- Arrows (grid 2-D, list up/down), PageUp/PageDown, Home/End. Enter: `Paste { plain: plain_by_default }` or
  `Insert { close: true }`; Shift+Enter: the other paste mode; Ctrl+Enter: `Copy`; Delete (query empty or caret at
  its end): `Delete`; Ctrl+P: `SetPinned`; Ctrl+1…9: paste that card (⌃1…⌃9 badges show while Control is held).
- Tab / Shift+Tab / Ctrl+Tab / Ctrl+Shift+Tab: `TabChanged` (+ `QueryChanged` for the new tab if a query is set).
- Esc: closes a menu/popover/clear confirmation, else clears the query, else posts `Close`.
- Mouse: click a card → `Paste` (Shift-click = other mode); right-click → menu (Paste, Paste as plain text / with
  formatting, Copy, Pin/Unpin, Copy text from image, Edit in Glint when `glint_available`, Show in Explorer, Delete).
  Hover shows pin/delete buttons. Click a picker cell → `Insert { close: close_after_click }`; right-click or long
  press on an emoji with tones → tone popover. Category buttons / chips spring-scroll to their section. Dragging the
  search-row background or the tab-bar gaps posts `DragTo(origin)`. Gear → `OpenSettings`. Bottom bar: "Clear" asks
  inline, then `ClearUnpinned`; "Resume" → `SetPaused(false)`.
- Timers (tokens 1–7) and window events are handled inside; the caret blinks only while open (idle 0 % otherwise).

## Typical wiring
```rust
let id = app.open(WindowSpec::panel(origin, Panel::SIZE).backdrop(mode).hidden(), Panel::new(options))?;
app.with_view(id, |p: &mut Panel, cx| { p.set_backdrop(mode); p.set_clips(cx, rows, total); cx.render_hidden() });
// hotkey: move the window, then
app.with_view(id, |p: &mut Panel, cx| { p.open(cx, Tab::Emoji); cx.show_window(false) });
// keys from the hook:
app.with_view(id, |p: &mut Panel, cx| { p.event(cx, &Event::KeyDown(key)); p.event(cx, &Event::Text(text)) });
// PanelAction::Close → p.close(cx); poll is_closing_done() from a timer → cx.hide_window()
```

## Previews (galleries, `tuck --preview`)
`preview_query(q)` (sets the field without posting), `preview_caret(Option<bool>)`, `preview_ctrl_held(b)`,
`preview_hover_card(Option<id>)`, `preview_select_card(id)`, `preview_menu(gfx, id, at, highlighted_row)`,
`preview_confirm_clear()`, `preview_hover_cell(gfx, tab, text, label)`, `preview_select_cell(gfx, tab, index)`,
`preview_tones(gfx, text, highlighted)`, `preview_skin_picker(gfx)`, `preview_scroll(gfx, tab, offset)`,
`preview_hover_gear()`. Render with `tuck_ui::render_view_offscreen` (or `render_offscreen` + `View::paint`).

## Verification
`cargo test -p tuck-panel` (formatting, layout, keyboard model). `cargo run -p tuck-panel --release --example
gallery -- --out <dir>` renders every state (clipboard default/ctrl/menu/scrolled/paused/clear/empty/no-results,
emoji/tones/skin-tone/scrolled/search, kaomoji, symbols) in dark and light at scale 1 and 2 over a synthetic blurred
wallpaper, plus `emoji-dark@4x.png`, and prints paint timings.
