# tuck-ui API

Direct2D/DirectWrite/DirectComposition render kit, single-threaded event loop, springs, Lucide icons, DESIGN §5
themes, widgets and color emoji. Everything is in DIPs (`f32`) unless a name ends in `_px` (physical pixels, virtual
desktop). Everything except `AppProxy` is `!Send`: one UI thread owns `App`, `Gfx`, windows and views.

## Startup and the loop
```rust
tuck_ui::enable_per_monitor_dpi_awareness();            // examples/tests; the app uses its manifest
tuck_ui::run(|app: &App| {                               // GetMessage-style loop until app.quit()
    app.on_event(|app, e: MyEvent| { /* typed app events */ });
    let proxy = app.proxy();                              // Send + Sync + Clone: proxy.post(MyEvent::X) -> bool
    app.open(WindowSpec::panel(origin_px, SizeF::new(380.0, 460.0)).hidden(), MyView::new())?;
    Ok(())
})?;
```
`App` (cheap `Rc` clone): `gfx()`, `proxy()`, `on_event::<E>(FnMut(&App, E))`, `post(e)` (queued, never re-entrant),
`open(spec, view) -> WindowId`, `close(id)` (deferred), `request_paint(id)`, `with_view::<V, R>(id, |v: &mut V, cx| ..)`
(downcast + run with a `Ctx`; how never-focused panels get keys), `hwnd(id)`, `window_ids()`, `set_timer(Duration,
FnOnce(&App)) -> TimerId`, `cancel_timer`, `quit()`, `set_quit_when_no_windows(bool)` (default false),
`set_appearance(ThemeMode, use_system_accent)`, `theme_for(ThemeMode)`, `system_prefers_dark()`.
Frames render only when asked (`request_paint`, `animate`, a moving `Animated`, an emoji still arriving). While
anything animates the loop waits on `DCompositionWaitForCompositorClock` (fallback `DwmFlush`); idle = blocked in
`MsgWaitForMultipleObjectsEx`, 0 % CPU. Modal loops entered from callouts keep frames, timers and events running.

## Windows — `WindowSpec`
- `overlay(rect_px)`: borderless topmost monitor cover, always dark, forced foreground on show. Alt+F4 → `CloseRequested`.
- `normal(title, size)`: resizable, Mica, themed title bar. `.centered_in(work_px)`, `.min_size(s)`, `.mica(b)`,
  `.fixed_size()`, `.unified_title_bar()` (return `HitArea::Caption` from `View::hit_test`; keep
  `cx.caption_buttons_rect()` free).
- `popup(origin_px, size)`: layered, per-pixel transparent, topmost, non-activating. `.topmost(b)`, `.activating()`,
  `.click_through()`; `View::interactive_region` makes only the returned rects clickable (cursor-polled
  `WS_EX_TRANSPARENT`, 30 Hz near the popup, ≤ 2 Hz far away).
- `panel(origin_px, size)` (DESIGN §9): `WS_POPUP` + `NOACTIVATE | TOPMOST | TOOLWINDOW | NOREDIRECTIONBITMAP`, not
  layered, window rect = content rect. Before the first show: DWM corners `Round`, border color none, transitions
  off, dark mode per theme and the backdrop, set once: `.backdrop(PanelBackdrop::SystemAcrylic)` (default,
  `DWMSBT_TRANSIENTWINDOW` + frame extended), `AccentAcrylic` (`SetWindowCompositionAttribute`, accent policy 4,
  tint ≈ transparent), `Solid` (none; `is_opaque()` → paint an opaque fill). After showing it sends itself
  `WM_NCACTIVATE(TRUE)` through `DefWindowProc` and answers every `WM_NCACTIVATE` as active. `PanelBackdrop::ALL`,
  `.name()`. `cargo run -p tuck-ui --example backdrop_probe -- --monitor-x X --monitor-y Y --seconds N --out DIR`
  (shows windows; main thread only) captures each mode over a test pattern to `<mode>.png` / `<mode>-context.png`.
- Common: `.theme(ThemeMode)`, `.hidden()` (show later with `cx.show_window`), `.exclude_from_capture()`.
Windows appear only after their first frame is committed (no flash). DComp flip-model premultiplied swapchains,
per-monitor-v2 DPI, transparent device loss.

## Views
```rust
pub trait View: Any {
    fn event(&mut self, cx: &mut Ctx, event: &Event) -> bool { false } // true = consumed
    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter);
    fn hit_test(&self, pos: PointF) -> HitArea { HitArea::Client }      // unified title bars only
    fn interactive_region(&self) -> Option<Vec<RectF>> { None }       // popups: clickable client rects
}
```
`Ctx`: `size()`, `bounds()`, `scale()`, `px()`, `snap(v)`, `theme()`, `time()`, `gfx()`, `app()` (None offscreen),
`window()`, `hwnd()`, `is_offscreen()`, `request_paint()`, `animate()`, `set_cursor`, `capture_pointer()` /
`release_pointer()`, `close()`, `set_window_rect_px`, `move_window_px`, `resize_window`, `show_window(activate)`,
`hide_window()`, `render_hidden()`, `set_window_opacity(f32)`, `set_topmost`, `set_title`, `minimize`,
`toggle_maximize`, `activate()`, `post(e)`, `set_timer(Duration, token) -> TimerId` (→ `Event::Timer(token)`),
`cancel_timer`, `show_tooltip(anchor, text, shortcut)` / `hide_tooltip()` / `hide_tooltip_for(anchor)`,
`set_ime_caret`, `measure_text`, `window_rect_px()`, `client_origin_px()`, `caption_buttons_rect()`.
`Ctx::offscreen(gfx, size, scale, theme, time)` builds one for tests and previews (window ops ignored).
`Event`: `PointerDown/Move/Up(PointerEvent)`, `PointerLeave`, `PointerCancel`, `Wheel(WheelEvent)`,
`KeyDown/KeyUp(KeyEvent)`, `Text(String)`, `Focus(bool)`, `Resized`, `ScaleChanged`, `ThemeChanged`, `Timer(u64)`,
`CloseRequested`, `Closed`, `Shown`. `PointerEvent { pos, screen_px, kind, id, button, buttons, pressure, mods,
click_count, history }`; `WheelEvent { pos, delta /* notches, +y = up */, precise, mods }`; `KeyEvent { key, vk,
mods, repeat }`, `Key::{Escape, Enter, Tab, Backspace, Delete, Left.., Home, End, PageUp, Control, Char('A'), F(n)..}`,
`ev.is(Key::Char('Z'), Modifiers::CTRL)`.

## Painter (DIPs; `p.context()` is the raw `ID2D1DeviceContext`)
- Info: `scale()`, `px()`, `snap(v)`, `snap_point`, `snap_rect`, `theme()`, `size()`, `bounds()`, `gfx()`.
- Shapes: `fill_rect`, `stroke_rect`, `fill_round_rect`, `stroke_round_rect`, `hairline_round_rect(r, radius, color,
  inset)` (1 physical px), `fill_ellipse`, `fill_circle`, `stroke_ellipse`, `line`, `polyline`, `fill_path`,
  `stroke_path`, `checkerboard(rect, cell, light, dark)`. Brushes: `Color`, `Brush::linear(start, end, &stops)`,
  `Brush::vertical`. `StrokeStyle::round()`, `::dashed(&[..])`. Paths: `PathBuilder::new().move_to()..build(&gfx)`,
  `Path::from_svg`, `bounds()`, `contains(pt)`.
- Bitmaps: `bitmap(&Bitmap, dest, src_px, opacity, Interpolation::{Nearest, Linear, Cubic})`.
- State: `clip_rect`, `clip_round_rect`, `clip_path`, `layer(opacity, ..)`, `masked(Brush, ..)` (alpha of a gradient,
  e.g. scroll-edge fades), `with_transform`, `translate`, `scale_around`.
- Effects: `shadow(rect, radius, &Shadow)`, `shadow_outside`, `glass(rect, radius, Option<&Backdrop>)` (DESIGN §5),
  `glass_opaque(rect, radius)` (solid sheet for menus/popovers over busy content), `fill_backdrop`.
- Text: `text(str, &TextStyle, color, rect) -> width` (single line, cap-height centered, ellipsis), `text_at`,
  `measure`, `layout(str, style, max_width) -> Option<Rc<TextLayout>>` (cached), `draw_layout` (color fonts on: emoji
  in text draw in color). `TextStyle::{caption 11, body 13, emphasized, title 15sb, large_title 22sb, countdown}` +
  `.size() .weight() .tabular() .centered() .align() .wrap() .line_height() .family(FontFamily::{Ui, Mono (Cascadia
  Mono/Consolas), Symbol (Segoe UI Symbol), Emoji})`. `TextLayout`: `width`, `height`, `baseline`, `line_count`,
  `caret_rect(i)`, `hit_test(pt)`, `cluster_boundaries()` (UTF-16 caret stops), `ink_bounds()`, `raw()`.
  `Gfx::ellipsize_middle(text, style, max_width)` (paths keep drive and file name).
- Icons: `icon(Icon::Pen, center, size, color)`, `icon_with_stroke`. `Icon::ALL`, `name()`. Lucide set incl. Search,
  Settings, Clipboard, Smile, Parentheses, Omega, Link, Mail, Pin, PinOff, File, Files, Folder, Clock, Hand, PawPrint,
  Apple, Car, Volleyball, Lightbulb, Heart, Flag, Shapes, Sigma, Euro, ArrowLeftRight, Quote, Languages, Type,
  SearchX, Sparkles, Image, Trash, Copy, ScanText, Pencil, AppWindow, PauseFill, X, Check, Chevron*, ….
- Emoji: `emoji(text, center, size)` / `emoji_with_opacity` draw Segoe UI Emoji's COLRv1 (Fluent) artwork `size` DIP
  tall: custom `IDWriteTextRenderer` → `TranslateColorGlyphRun` (paint trees, then COLRv0 layers, bitmaps, SVG,
  outlines) → `DrawPaintGlyphRun`. Each (text, px size) is rasterized once into 1024² atlas pages (≤ 6, LRU page
  replacement); frames blit. COLRv1 costs ~5 ms CPU per glyph, so windows rasterize on background threads (software
  D2D) and emoji fade in 120 ms after they land (frames continue meanwhile); offscreen renders rasterize
  synchronously. `prepare_emoji(&texts, size)` batches a grid's misses (urgent queue); `emoji_size_px(size)`;
  `Gfx::prewarm_emoji(&texts, size_px)` queues work ahead of time; `emoji_cache_len()`, `emoji_pending()`.

## Bitmaps, offscreen, Gfx
`Bitmap::new(Image) -> Rc<Bitmap>` (straight BGRA, premultiplied on upload, re-uploaded after device loss),
`.blurred(sigma_px, saturation)`, `.glass_backdrop(theme, px_per_dip)`, `.read_back(&gfx)`, `.image()`, `.width()`.
`render_offscreen(&gfx, &OffscreenSpec, |cx, p| ..) -> Result<Image>`, `render_view_offscreen(&gfx, &mut view, &spec)`:
identical to on-screen output. `OffscreenSpec::new(size, scale, theme)` / `::pixels(w, h, scale, theme)`, `.time(t)`,
`.background(c)`. `Gfx::new()` (WARP fallback), `new_software()`, `shared()`, `factory()`, `dwrite()`, `wic()`,
`context()`, `d3d_device()`, `generation()`, `text_layout()`, `measure_text()`, `font_families()`, `is_software()`.

## Animation and theme
`Animated<T>` (`f32`, `PointF`, `SizeF`, `RectF`, `Color`): `new` (spring 420/0.86), `snappy` (700/0.9), `fade` (140 ms),
`with_motion(v, Motion::Tween(..))`; `.set(t)` (velocity carried), `.set_with`, `.snap`, `.get()`, `.target()`,
`.is_animating()`. `anim::reduced_motion()` snaps everything; `anim::with_clock(t, ..)`, `anim::now()`.
`Theme::dark()/light()/resolve()/with_accent()`: `glass_fill(_solid)`, `hairline`, `outer_border`, `shadows`, `text`,
`text_secondary`, `text_tertiary`, `hover`, `pressed`, `selected`, `accent`, `destructive`, `success`, `separator`,
`control_track`, `knob`, `window_background`, `on_accent`, `overlay_dim`. `Color::hex/rgba/rgba8/with_alpha/lerp/over`.

## Widgets (`tuck_ui::widgets`) — retained structs owned by your view
Lay out (`set_rect` / `layout`), route events (`event(cx, ev) -> Response<T>`: `Ignored | Consumed | Action(T)`),
`paint(p)`. Route to open menus/popovers first. `force_state(hovered, pressed)` snaps visuals for previews.
- `IconButton::new(Icon).label(..).tooltip(t, Some(key)).tint(c).icon_size(15.0).disabled().with_selected(b)`.
- `Segmented::new(vec![Segment::icon_label(Icon, "Emoji").tooltip(..)], sel).style(SegmentedStyle::Track)
  .compact()` (16 DIP icons, 12 DIP labels); `Action(index)`; the pill springs; `item_rect(i)`, `clear_selection()`.
- `Button::new("Clear", ButtonStyle::Primary|Secondary|Plain|PlainDestructive|Destructive)`, 28 high.
- `TextField::new("Search").icon(Icon::Search).style(s)`: `text()`, `set_text`, `set_placeholder`, `insert`,
  `delete_backward/forward(gfx, word)`, `move_caret(gfx, forward, word, extend)`, `move_to_edge`, `select_all`,
  `clear`, `caret_at_end`, `selection`; `event` (Text, keys via `key(gfx, &KeyEvent)`, click/drag, clear button) →
  `Action(())` when the text changed. Caret: accent 1.5 DIP, `BLINK` 530 ms, solid while typing; schedule repaints
  with `next_blink()`; `set_focused`, `restart_blink`, `force_caret(Some(true))` for previews. Grapheme-safe.
- `ScrollView::new(timer_token)`: `set_viewport`, `set_content_height`, `offset()`, `target()`, `max_offset()`,
  `scroll_to(cx, y, animate)`, `snap_to`, `reveal(cx, top, bottom, margin)`, `to_content(pos)`, `to_window(rect)`,
  `visible_range()`, `event` (wheel: spring per notch, touchpad 1:1; draggable thumb; `Timer(token)` fades the bar),
  `paint_content(p, |p| ..)` (clip + translate), `paint_scrollbar(p)`, `flash_scrollbar`, `force_scrollbar(opacity)`.
- `Menu::new(vec![MenuItem::new("Delete").icon(..).shortcut("Del").destructive(), MenuItem::separator()]).opaque()`,
  `open(gfx, anchor, bounds)`, `force_open(.., Some(row))`, `Action(index)`, ↑↓ Enter Esc, `highlighted()`.
- `Popover::new(content_size).opaque()`, `.placement`, `open(anchor, bounds)`, `force_open`, `content_rect()`,
  `paint(p, backdrop, |p, r| ..)`, `Action(())` = dismissed.
- `Toolbar`, `Toggle`, `Slider`, `ColorSwatch`, `Badge`, `Countdown`, `Presence` (DESIGN appear/disappear),
  `Interaction` (hover/press amounts) as before. `Tooltip::paint_bubble(..)`, `paint_bubble_above(p, anchor, text,
  opacity, bounds)`, `frame(..)`, `frame_above(..)`.

## Window helpers (`tuck_ui::win`, raw HWNDs as `isize`)
`set_mica`, `set_dark_mode`, `set_corner_preference`, `set_border_color(w, Option<Color>)`, `set_transient_acrylic`,
`set_accent_acrylic(w, tint)`, `activate_frame` (`WM_NCACTIVATE(TRUE)` via `DefWindowProc`), `disable_transitions`,
`extend_frame_into_client`, `set_exclude_from_capture`, `set_topmost`, `show_no_activate`, `set_window_rect_px`,
`window_rect_px`, `client_origin_px`, `caption_buttons_rect_px`, `dpi_for_window`, `dpi_at_point`, `dpi_for_rect`,
`work_area_at_point`, `force_foreground`, `utc_offset_minutes()` (local − UTC now). `Cursor::{Arrow, Hand, IBeam,
Crosshair, Move, Resize*, NotAllowed, Wait, Hidden, ..}`.

Verify visuals with `cargo run -p tuck-ui --release --example gallery -- --out <dir>` (PNGs, no window).
