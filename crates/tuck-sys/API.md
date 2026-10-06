# tuck-sys

Windows plumbing (DESIGN §3, §4). Rects/points are physical screen pixels; images are straight-alpha BGRA
(`tuck_core::Image`). Fallible calls return `anyhow::Result`. Nothing starts at import time. Tests never install
hooks, inject input, touch the clipboard, write the registry or open windows.

## Input hook (`hook`)

- `InputHook::start(HookConfig, sink) -> Result<InputHook>`: hook thread (TIME_CRITICAL, own message loop) with
  `WH_KEYBOARD_LL` always and `WH_MOUSE_LL` only while capturing; `sink: Fn(HookEvent) + Send + Sync` runs on a
  dispatcher thread (HIGHEST) — post to the UI loop from there. Drop stops and joins both threads.
- `set_config(HookConfig)`; `HookConfig::from(&settings.shortcuts)` (`win_v`, `win_period`).
- `begin_capture(panel_rect_px)`, `set_panel_rect(rect)`, `end_capture()`, `is_capturing()`.
- `heartbeat()`: call at least every 250 ms while capturing. A key or click arriving more than
  `HEARTBEAT_TIMEOUT_MS` (1000) after the last heartbeat ends capture, passes through and sends `Dismiss(Stalled)`;
  `is_capturing()` then turns false on its own.
- `on_session_unlock()`: reinstall now (also every 10 minutes). Call from the WTS_SESSION_UNLOCK handler.
- `MARKER = 0x5455434B`: `dwExtraInfo` of everything Tuck injects; the hook ignores those events.
- `HookEvent::{Hotkey(Hotkey), Key(RoutedKey), Dismiss(DismissReason)}`;
  `Hotkey::{Clipboard, Emoji}`; `DismissReason::{ClickOutside, Shortcut, Stalled}`.
- `RoutedKey { vk, scan, extended, down, repeat, mods: Mods, caps_lock }`;
  `Mods { shift, ctrl, alt, win, altgr }` are effective: AltGr (RAlt, incl. its LCtrl) is `altgr` only, and the
  Win key that was down when capture began reads as up until released (`opening_win`).

Routing (always): Win+V → `Hotkey(Clipboard)`, Win+. / Win+; → `Hotkey(Emoji)`, only with Win alone; down, repeats
and up are swallowed and the mask key (VK 0xE8, tagged) is injected so Start does not open.

Routing while capturing:

| key | result |
|---|---|
| Shift/Ctrl/Alt/Win (L/R) | pass; first down and the up are also sent as `Key` (for ⌃ badges) |
| Caps/Num/Scroll Lock, media/volume/browser/launch keys, Sleep, PrintScreen, IME mode keys, 0xE8/0xFF | pass, no event |
| F1–F24 | pass + `Dismiss(Shortcut)` |
| Ctrl+Backspace, Ctrl+A, Ctrl+1…9, Ctrl+P, Ctrl+Tab, Ctrl+Shift+Tab, Ctrl+Enter | swallow, `Key` |
| any other key with effective Ctrl, Alt or Win | pass + `Dismiss(Shortcut)` |
| everything else (plain, Shift, AltGr, VK_PACKET) | swallow, `Key`; repeats sent with `repeat: true` |
| mouse button down outside the panel rect | pass + `Dismiss(ClickOutside)` |

Key-ups follow their down: swallowed → swallowed (routed ups are sent while capturing), passed → passed; keys held
before capture pass their repeats and up; a routed key still held after `end_capture` stays swallowed silently.
When a routed key is swallowed while Win or Alt is physically down, the mask key is injected too.

`HookState` is the pure state machine (`on_key(KeyInput) -> Decision`, `on_button_down(PointI, now_ms)`,
`begin_capture(rect, now_ms)`, `heartbeat(ms)`, `release_stale_keys`, `reset`); tests drive it with fixed times.

## Text (`keys`)

- `TextTranslator::new()`, `translate(&RoutedKey, hkl: isize) -> KeyText`, `reset()`. Pass `Target::layout`.
- `KeyText::{None, Text(String), Dead}`. Uses `ToUnicodeEx(.., 0x4, hkl)` with a key state synthesized from
  `mods` + `caps_lock` (AltGr = Ctrl+Alt+RMenu); never changes the system keyboard state or layout.
- Dead keys: `Dead` is returned and remembered; the next letter is composed with `NormalizeString(NormalizationC)`
  (`^`+`e` → `ê`); dead + space → the dead char; dead + uncombinable key (or another dead key) → dead char + that
  key's text. Control characters (Enter, Tab, Backspace, Esc, Ctrl+letter) are `None` and clear a pending dead key.
  Key-ups are `None`. `VK_PACKET` keys (text other tools inject) yield their character.
- Call `reset()` when the panel opens or the target changes.

## Injection (`inject`)

- `type_text(&str) -> Result<()>`: waits up to 300 ms (5 ms polls) for Shift/Ctrl/Alt/Win to be released, injects
  key-ups only for modifiers still down (mask key first if Win or Alt is among them; never re-pressed), then one
  `SendInput` batch of `KEYEVENTF_UNICODE` down/up pairs per UTF-16 unit; `\r\n`, `\n`, `\r` become Enter.
- `paste(PasteKey) -> Result<()>`: same modifier handling, then Ctrl+V / Shift+Insert (extended) / Ctrl+Shift+V.
- Both block up to ~300 ms; everything is tagged with `MARKER`. Elevated targets ignore the input silently (UIPI):
  check `Target::elevated` first.

## Target (`target`)

- `capture_target() -> Option<Target>` (~0.2 ms, no cross-process messages):
  `Target { hwnd, focus, thread_id, pid, exe: Option<PathBuf>, layout, caret: Option<RectI>, work_area, elevated }`.
  The caret comes from `GetGUIThreadInfo`, mapped to physical pixels in the target's DPI context (correct for
  DPI-virtualized apps). `work_area` is the caret's monitor, else the window's. `elevated` reads `TokenElevation`
  (false when unreadable).
- `Target::is_foreground()`, `Target::exe_name()` (lowercase file name for `PasteSettings::key_for` / ignored apps).
- `process_exe(pid) -> Option<PathBuf>`, `work_area_at(PointI) -> RectI`, `scale_at(PointI) -> f32` (1.0 = 96 dpi),
  `cursor_pos() -> PointI`.
- `ForegroundWatch::start(sink: impl Fn(isize) + 'static) -> Result<_>`: `sink(new_foreground_hwnd)` on the calling
  (UI) thread, which must pump messages. Drop unhooks; not `Send`.

## Caret (`caret`)

- `CaretLocator::start() -> Result<_>`: MTA worker with a warm `CUIAutomation8` (UIA timeouts 1 s).
- `locate(&Target, budget: Duration) -> Option<Placement>`: `target.caret` → `Placement::Caret` immediately; else
  the worker: MSAA `OBJID_CARET` (Chromium windows — `Chrome_WidgetWin_*`, `Chrome_RenderWidgetHostHWND` — stop
  here), then UIA `TextPattern2::GetCaretRange` (TextPattern selection as fallback) → `Caret`, then the focused
  element's bounds when under 80 % of the work area → `Element`. A late answer is dropped and that pid skips the
  worker for 60 s; while the worker is still busy with an older request, `locate` returns `None` at once.
  Measured: ~13 ms for the first worker call, ~1.5 ms after.
- `Placement::{Caret(RectI), Element(RectI)}`.
- `panel_origin(placement, cursor, panel_px: (w, h), work_area, scale) -> PointI` (DESIGN §8, gaps are DIP ×
  `scale`): caret → below with 6 DIP gap, left edge caret.x − 24 DIP, above when no room; element → its bottom-left
  with the same gap/flip; `None` → 12 DIP below-right of the cursor, flipped left/up when it does not fit. Always
  clamped to `work_area` with an 8 DIP margin.

## Apps (`apps`)

- `display_name(&Path) -> String`: version resource FileDescription (translation table, then en-US fallbacks), else
  the file stem. Cached.
- `icon(&Path, size_px) -> Option<Image>`: `SHDefExtractIconW` at `size_px` (max 256), else the shell's large icon;
  icons without alpha get it from the AND mask. Cached per (exe, size). Initialize COM on the calling thread.

## Ported from Glint

- `tray`: `Tray::add(hwnd, callback_msg, HICON, tooltip)`, `set_tooltip`, `set_icon`, `recreate` (on
  `taskbar_created_message()`), Drop removes it. `MenuItem::{new, separator, submenu}` + `.checked/.enabled`,
  `show_context_menu(hwnd, &items, PointI) -> Option<u32>`, `set_dark_menus(bool)`.
- `instance`: `acquire_single_instance() -> InstanceRole::{Primary(InstanceGuard), Secondary}` (mutex
  `Local\Tuck.SingleInstance`); create a message-only window of class `IPC_WINDOW_CLASS` (`Tuck.Ipc`);
  `forward_to_primary(&[String]) -> Result<bool>` (JSON over WM_COPYDATA, 2 s timeout, ≤ 1 MiB);
  `parse_copydata(LPARAM) -> Option<Vec<String>>` inside WM_COPYDATA (return nonzero when accepted).
- `install`: `installed_exe_path()` (`%LOCALAPPDATA%\Programs\Tuck\tuck.exe`), `is_installed()`,
  `install(current_exe, &Settings) -> Result<PathBuf>` (copy, HKCU Run `Tuck` = `"<exe>" --background` when
  `settings.launch_at_login`, Start-menu `Tuck.lnk`, HKCU Uninstall `Tuck`: DisplayName, DisplayIcon,
  DisplayVersion, Publisher `joxan2137`, InstallLocation, UninstallString `"<exe>" --uninstall`, NoModify,
  NoRepair, EstimatedSize; initialize COM first), `set_launch_at_login(bool, exe)`, `uninstall(purge)` (reverses
  it; `purge` also deletes `%LOCALAPPDATA%\Tuck` and `%APPDATA%\Tuck`; when the installed copy uninstalls itself a
  detached helper deletes it ~2 s after exit, so exit promptly).
- `logging::init_logging(&Path)`: file logger (rotates at 1 MiB to `.1`) + panic hook, once per process; use
  `paths::log_path()`.
- `paths`: `local_data_dir()` (`%LOCALAPPDATA%\Tuck`), `settings_dir()` (`%APPDATA%\Tuck`, or `TUCK_DATA_DIR`),
  `history_dir()` (local `\history`), `temp_dir()` (`%TEMP%\Tuck`), `log_path()`; directories are created.
- `settings_store`: `settings_path()`, `load_settings() -> Settings` (defaults when missing; invalid JSON is moved
  to `.json.bak`), `save_settings(&Settings)` (temp file + atomic replace).
- `shell`: `open_path(&Path)`, `open_uri(&str)`, `reveal_in_explorer(&Path)` (COM initialized).
- `ocr`: `recognize(&Image) -> Result<String>` (blocking WinRT OCR, lines joined by `\n`; run on a worker),
  `ocr_available() -> bool`.

## Verify

`cargo test -p tuck-sys`; `cargo run -p tuck-sys --example probe -- [delay s]` prints the foreground target, both
caret paths with timings, the app name and icon size (read-only: no hook, window or input).
