//! The resident instance (DESIGN §8): tray, input hook, the never-activated panel window and its open/close flow, the
//! clipboard service and history (the store on its own thread), the emoji catalogs, the settings window and toasts.
//! All state lives in one `Rc<RefCell<State>>` on the UI thread; hook, clipboard, store, catalog, icon, OCR and
//! injection work run on other threads and post results back through the `AppProxy`. Borrows of the state are kept
//! short and never span a modal call (the tray menu), a window creation or a call into a view.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tuck_clip::{CaptureFilter, Captured, ClipEvent, ClipboardService, WritePayload};
use tuck_core::settings::PasteSettings;
use tuck_core::{AddOutcome, ClipContent, ClipId, History, Image, Op, PasteKey, Settings, SourceApp, now_ms};
use tuck_emoji::{Coverage, Kind, Usage};
use tuck_panel::{Panel, PanelAction, PickerSection, Tab};
use tuck_store::{Protection, blob_name};
use tuck_sys::target::{cursor_pos, scale_at, work_area_at};
use tuck_sys::{
    CaretLocator, ForegroundWatch, HookConfig, HookEvent, Hotkey, InputHook, KeyText, Placement, RoutedKey, Target,
    TextTranslator, capture_target, install, panel_origin, paths, settings_store, shell,
};
use tuck_ui::{
    App, AppProxy, Bitmap, Ctx, Event, Key, KeyEvent, Modifiers, Painter, PanelBackdrop, PointI, RectI, TimerId, View,
    WindowId, WindowSpec,
};

use crate::cli::{self, Command, Greeting};
use crate::clips::{self, RowCache};
use crate::host::{HostMessage, HostWindows};
use crate::pickers;
use crate::popup::corner_layout;
use crate::settings_model::{
    GITHUB_URL, ImportState, InstallState, SettingsEnv, SettingsRequest, effects, panel_options,
};
use crate::settings_view::{MIN_SIZE, SettingsMessage, SettingsView, WINDOW_SIZE};
use crate::store::{StoreEvent, StoreJob, StoreThread};
use crate::system::{OleGuard, glint_exe, hwnd, is_window, join_bounded, raise_ui_priority, running_apps};
use crate::toast::{Tint, Toast, ToastClosed, ToastIcon, ToastView};
use crate::tray::{TrayCommand, TrayIcon, command_for, menu_items};
use crate::workers::{IconWorker, Injection, Injector};

/// The DWM material behind the panel (DESIGN §9). Chosen once at window creation; the live backdrop probe decides
/// the shipped value.
/// DWM's system acrylic turns solid on a window that is never activated; the accent policy keeps blurring.
pub const PANEL_BACKDROP: PanelBackdrop = PanelBackdrop::AccentAcrylic;

/// How long the UIA/MSAA caret lookup may delay the panel.
const CARET_BUDGET: Duration = Duration::from_millis(20);
/// The hook releases the keyboard after 1000 ms without a heartbeat.
const HEARTBEAT: Duration = Duration::from_millis(200);
const HIDE_POLL: Duration = Duration::from_millis(16);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);
const USAGE_SAVE_DEBOUNCE: Duration = Duration::from_secs(2);
const TRAY_RETRY: Duration = Duration::from_secs(2);
const TRAY_ATTEMPTS: u32 = 60;
const QUIT_WORKER_WAIT: Duration = Duration::from_secs(3);
const END_SESSION_WORKER_WAIT: Duration = Duration::from_secs(2);

/// Everything other threads and windows post to the UI thread's main handler.
enum AppEvent {
    Host(HostMessage),
    /// A hook event and when the hook's dispatcher saw it.
    Hook(HookEvent, Instant),
    Copied {
        captured: Box<Captured>,
        source: Option<SourceApp>,
    },
    Written {
        result: Result<()>,
        after: AfterWrite,
    },
    CatalogsReady(Box<Usage>),
    CoverageReady(Coverage),
    AppIcon(PathBuf, Option<Image>),
    TextRecognized(Result<String>),
    Imported(Result<Vec<Captured>>),
    InjectionFailed(String),
    GlintFailed(anyhow::Error),
    Foreground(isize),
}

/// The panel window reached the screen.
struct PanelShown;

/// Posted to leave the loop after the current handler returned.
struct QuitRequest;

/// How a picked item reaches the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delivery {
    /// Typed or pasted into the window that had the foreground.
    Paste(PasteKey),
    /// Left on the clipboard.
    Copy(CopyReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopyReason {
    /// Ctrl+Enter or "Copy".
    Asked,
    /// Opened from the tray: there is no window to paste into.
    NoTarget,
    /// The target runs as administrator; injected input would be dropped (UIPI).
    Elevated,
}

fn delivery(target: Option<&Target>, paste: &PasteSettings) -> Delivery {
    match target {
        None => Delivery::Copy(CopyReason::NoTarget),
        Some(target) if target.elevated => Delivery::Copy(CopyReason::Elevated),
        Some(target) => Delivery::Paste(paste.key_for(target.exe_name().as_deref())),
    }
}

/// What happens once the clipboard holds a write.
enum AfterWrite {
    /// The paste keys were sent; the item moves to the top.
    Pasted(ClipId),
    /// Left for the user to paste; a history item moves to the top.
    Copied { id: Option<ClipId>, reason: CopyReason },
    /// Text read from an image becomes a history item.
    Recognized(String),
}

/// What a blob read from the store is for.
enum ImageUse {
    Write { id: ClipId, delivery: Delivery },
    Ocr,
    Glint { id: ClipId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Opener {
    Hotkey,
    Command,
    /// Tray icon or menu: copy only, placed at the cursor (the tray corner).
    Tray,
}

/// The open panel and where its keys and picks go.
struct Session {
    /// None: copy instead of paste.
    target: Option<Target>,
    /// Keyboard layout for typed search text (the target's, else the foreground's).
    layout: isize,
    key_at: Instant,
    generation: u64,
}

/// The panel view plus the one thing the app needs from the window: knowing when it reached the screen.
struct PanelHost {
    panel: Panel,
}

impl View for PanelHost {
    fn event(&mut self, cx: &mut Ctx, event: &Event) -> bool {
        if matches!(event, Event::Shown) {
            cx.post(PanelShown);
        }
        self.panel.event(cx, event)
    }

    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter) {
        self.panel.paint(cx, p);
    }
}

struct State {
    settings: Settings,
    hook: Option<InputHook>,
    tray: TrayIcon,
    host: Option<HostWindows>,
    owner: isize,
    session: Option<Session>,
    session_generation: u64,
    translator: TextTranslator,
    locator: Option<CaretLocator>,
    foreground: Option<ForegroundWatch>,
    history: History,
    clipboard: Option<ClipboardService>,
    store: StoreThread,
    injector: Option<Injector>,
    icons: Option<IconWorker>,
    rows: RowCache,
    clip_query: String,
    usage: Usage,
    coverage: Coverage,
    catalogs_ready: bool,
    /// Full sections per picker tab (`Kind::ALL` order), rebuilt when usage or coverage changes.
    full_sections: [Vec<PickerSection>; 3],
    /// The tab shows search results instead of its full sections (`Kind::ALL` order).
    showing_results: [bool; 3],
    /// Usage or coverage changed while the panel was open; rebuild once it hides.
    pickers_stale: bool,
    pending_images: HashMap<u64, ImageUse>,
    next_ticket: u64,
    write_token: u64,
    toast: Option<(u64, WindowId)>,
    next_toast_id: u64,
    settings_window: Option<WindowId>,
    import: ImportState,
    save_timer: Option<TimerId>,
    usage_timer: Option<TimerId>,
    relaunch: Option<PathBuf>,
    workers: Vec<JoinHandle<()>>,
}

/// What `main` does after the loop ends.
pub enum Exit {
    Done,
    /// `--install` from the settings window: start the installed copy once this process released the mutex.
    Relaunch(PathBuf),
}

#[derive(Clone)]
struct Tuck {
    state: Rc<RefCell<State>>,
    proxy: AppProxy,
    panel: WindowId,
}

/// Runs the resident instance until the user quits. `initial` is the command that started it.
pub fn run(initial: Command) -> Result<Exit> {
    let started = Instant::now();
    let _ole = OleGuard::new();
    let settings = settings_store::load_settings();
    let relaunch: Rc<RefCell<Option<PathBuf>>> = Rc::default();
    let relaunch_out = relaunch.clone();
    raise_ui_priority();
    tuck_ui::run(move |app: &App| {
        app.set_quit_when_no_windows(false);
        app.set_appearance(settings.theme, false);
        let proxy = app.proxy();
        let panel = open_panel_window(app, &settings)?;
        let sink = proxy.clone();
        let host = HostWindows::create(move |message| {
            sink.post(AppEvent::Host(message));
        })?;
        let owner = host.notify_hwnd().0 as isize;
        let store_sink = proxy.clone();
        let store = StoreThread::start(
            paths::local_data_dir()?,
            Protection::Dpapi,
            settings.history.keep_after_restart,
            move |event| {
                store_sink.post(event);
            },
        )?;
        let tuck = Tuck { state: Rc::new(RefCell::new(State::new(settings, owner, host, store))), proxy, panel };
        tuck.register(app, relaunch.clone());
        tuck.show_tray_with_retry(app, TRAY_ATTEMPTS);
        let hook_ok = tuck.start_hook();
        log::info!("hooks live {:.0} ms after start", started.elapsed().as_secs_f64() * 1000.0);
        tuck.start_helpers(app);
        tuck.load_catalogs();
        tuck.run_command(app, initial, hook_ok);
        Ok(())
    })?;
    let relaunch = relaunch_out.borrow_mut().take();
    Ok(relaunch.map_or(Exit::Done, Exit::Relaunch))
}

/// The panel window, created hidden with its first frame rendered so the first hotkey only moves and shows it.
fn open_panel_window(app: &App, settings: &Settings) -> Result<WindowId> {
    let work = work_area_at(cursor_pos());
    let spec = WindowSpec::panel(PointI::new(work.x, work.y), Panel::SIZE).backdrop(PANEL_BACKDROP).hidden();
    let panel = Panel::new(panel_options(settings, glint_exe().is_some()));
    let window = app.open(spec, PanelHost { panel }).context("creating the panel window")?;
    app.with_view(window, |host: &mut PanelHost, cx| {
        host.panel.set_backdrop(PANEL_BACKDROP);
        cx.render_hidden();
    });
    Ok(window)
}

impl State {
    fn new(settings: Settings, owner: isize, host: HostWindows, store: StoreThread) -> Self {
        Self {
            settings,
            hook: None,
            tray: TrayIcon::new(owner),
            host: Some(host),
            owner,
            session: None,
            session_generation: 0,
            translator: TextTranslator::new(),
            locator: None,
            foreground: None,
            history: History::new(),
            clipboard: None,
            store,
            injector: None,
            icons: None,
            rows: RowCache::default(),
            clip_query: String::new(),
            usage: Usage::default(),
            coverage: Coverage::all_supported(),
            catalogs_ready: false,
            full_sections: Default::default(),
            showing_results: [false; 3],
            pickers_stale: false,
            pending_images: HashMap::new(),
            next_ticket: 0,
            write_token: 0,
            toast: None,
            next_toast_id: 0,
            settings_window: None,
            import: ImportState::Idle,
            save_timer: None,
            usage_timer: None,
            relaunch: None,
            workers: Vec::new(),
        }
    }
}

/// The panel's view of a routed key: AltGr is text, not Ctrl+Alt (the hook already folds it).
fn key_event(key: &RoutedKey) -> KeyEvent {
    let mods = Modifiers { shift: key.mods.shift, ctrl: key.mods.ctrl, alt: key.mods.alt, win: key.mods.win };
    KeyEvent { key: Key::from_vk(key.vk), vk: key.vk, mods, repeat: key.repeat }
}

fn kind_slot(kind: Kind) -> usize {
    Kind::ALL.iter().position(|k| *k == kind).unwrap_or(0)
}

fn usage_path() -> Result<PathBuf> {
    Ok(paths::local_data_dir()?.join("usage.json"))
}

fn load_usage() -> Usage {
    let Ok(path) = usage_path() else { return Usage::default() };
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            log::warn!("usage.json is unreadable, starting fresh: {error}");
            Usage::default()
        }),
        Err(_) => Usage::default(),
    }
}

fn save_usage(usage: &Usage) -> Result<()> {
    let path = usage_path()?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec(usage)?)
        .with_context(|| format!("writing {}", temporary.display()))?;
    std::fs::rename(&temporary, &path).with_context(|| format!("replacing {}", path.display()))
}

fn spawn_worker(name: &str, work: impl FnOnce() + Send + 'static) -> Option<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(work)
        .map_err(|error| log::error!("could not start {name}: {error}"))
        .ok()
}

fn exe_for_login() -> PathBuf {
    if install::is_installed()
        && let Ok(path) = install::installed_exe_path()
    {
        return path;
    }
    std::env::current_exe().unwrap_or_default()
}

fn first_line(text: &str, max_chars: usize) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut short: String = line.chars().take(max_chars).collect();
    if line.chars().count() > max_chars {
        short.push('…');
    }
    Some(short)
}

fn error_text(error: &anyhow::Error) -> String {
    first_line(&format!("{error:#}"), 90).unwrap_or_else(|| "Unknown error".into())
}

/// Writes `image` as a PNG in `%TEMP%\Tuck` and opens it with `glint --edit`.
fn edit_in_glint(id: ClipId, image: &Image) -> Result<()> {
    let glint = glint_exe().context("Glint is not installed")?;
    let path = paths::temp_dir()?.join(format!("{id}.png"));
    std::fs::write(&path, tuck_clip::formats::encode_png(image)?)
        .with_context(|| format!("writing {}", path.display()))?;
    std::process::Command::new(&glint)
        .arg("--edit")
        .arg(&path)
        .spawn()
        .with_context(|| format!("starting {}", glint.display()))?;
    Ok(())
}

fn copied_toast(reason: CopyReason) -> Toast {
    let detail = match reason {
        CopyReason::Asked => None,
        CopyReason::NoTarget => Some("Paste it with Ctrl + V"),
        CopyReason::Elevated => Some("That app runs as administrator; paste with Ctrl + V"),
    };
    Toast::new(ToastIcon::Symbol(tuck_ui::Icon::Copy, Tint::Accent), "Copied", detail)
}

impl Tuck {
    fn register(&self, app: &App, relaunch: Rc<RefCell<Option<PathBuf>>>) {
        let tuck = self.clone();
        app.on_event(move |app: &App, event: AppEvent| tuck.on_event(app, event));
        let tuck = self.clone();
        app.on_event(move |app: &App, event: StoreEvent| tuck.on_store(app, event));
        let tuck = self.clone();
        app.on_event(move |app: &App, action: PanelAction| tuck.on_panel(app, action));
        let tuck = self.clone();
        app.on_event(move |_: &App, PanelShown: PanelShown| {
            if let Some(session) = &tuck.state.borrow().session {
                log::info!("panel on screen {:.1} ms after the key", session.key_at.elapsed().as_secs_f64() * 1000.0);
            }
        });
        let tuck = self.clone();
        app.on_event(move |_: &App, ToastClosed(id): ToastClosed| {
            let mut state = tuck.state.borrow_mut();
            if state.toast.is_some_and(|(open, _)| open == id) {
                state.toast = None;
            }
        });
        let tuck = self.clone();
        app.on_event(move |app: &App, message: SettingsMessage| tuck.on_settings(app, message));
        let tuck = self.clone();
        app.on_event(move |app: &App, QuitRequest: QuitRequest| {
            *relaunch.borrow_mut() = tuck.state.borrow_mut().relaunch.take();
            app.quit();
        });
        let state = Rc::downgrade(&self.state);
        let proxy = self.proxy.clone();
        let panel = self.panel;
        crate::host::set_end_session_handler(move || {
            if let Some(state) = state.upgrade() {
                Tuck { state, proxy: proxy.clone(), panel }.end_session();
            }
        });
    }

    fn with_panel<R>(&self, app: &App, f: impl FnOnce(&mut Panel, &mut Ctx) -> R) -> Option<R> {
        app.with_view(self.panel, |host: &mut PanelHost, cx| f(&mut host.panel, cx))
    }

    fn settings(&self) -> Settings {
        self.state.borrow().settings.clone()
    }

    fn track_worker(&self, handle: Option<JoinHandle<()>>) {
        if let Some(handle) = handle {
            let mut state = self.state.borrow_mut();
            state.workers.retain(|w| !w.is_finished());
            state.workers.push(handle);
        }
    }

    // ---- startup ----------------------------------------------------------------------------------------------

    fn start_hook(&self) -> bool {
        let config = HookConfig::from(&self.state.borrow().settings.shortcuts);
        let proxy = self.proxy.clone();
        match InputHook::start(config, move |event| {
            proxy.post(AppEvent::Hook(event, Instant::now()));
        }) {
            Ok(hook) => {
                self.state.borrow_mut().hook = Some(hook);
                true
            }
            Err(error) => {
                log::error!("input hook failed: {error:#}");
                false
            }
        }
    }

    /// Injection and icon threads, the caret locator and the foreground watch.
    fn start_helpers(&self, app: &App) {
        let proxy = self.proxy.clone();
        let injector = Injector::start(move |message| {
            proxy.post(AppEvent::InjectionFailed(message));
        });
        let proxy = self.proxy.clone();
        let icons = IconWorker::start(move |exe, image| {
            proxy.post(AppEvent::AppIcon(exe, image));
        });
        let locator = CaretLocator::start();
        let poster = app.clone();
        let foreground = ForegroundWatch::start(move |window| poster.post(AppEvent::Foreground(window)));
        let mut state = self.state.borrow_mut();
        state.injector = injector.map_err(|error| log::error!("{error:#}")).ok();
        state.icons = icons.map_err(|error| log::error!("{error:#}")).ok();
        state.locator = locator.map_err(|error| log::warn!("caret locator unavailable: {error:#}")).ok();
        state.foreground = foreground.map_err(|error| log::warn!("foreground watch unavailable: {error:#}")).ok();
    }

    fn start_clipboard(&self) {
        let filter = CaptureFilter::from(&self.state.borrow().settings.history);
        let proxy = self.proxy.clone();
        let started = ClipboardService::start(filter, move |event| match event {
            ClipEvent::Captured(captured) => {
                let source = captured
                    .source_exe
                    .as_ref()
                    .map(|exe| SourceApp { exe: exe.clone(), name: tuck_sys::apps::display_name(exe) });
                proxy.post(AppEvent::Copied { captured: Box::new(captured), source });
            }
            ClipEvent::OwnWrite { .. } => {}
            ClipEvent::Skipped(reason) => log::debug!("copy skipped: {reason:?}"),
        });
        match started {
            Ok(service) => self.state.borrow_mut().clipboard = Some(service),
            Err(error) => log::error!("clipboard history unavailable: {error:#}"),
        }
    }

    /// Parses the catalogs and reads usage on a worker, then works out which emoji the font draws.
    fn load_catalogs(&self) {
        let proxy = self.proxy.clone();
        let handle = spawn_worker("tuck-catalogs", move || {
            let started = Instant::now();
            let counts: Vec<usize> = Kind::ALL.iter().map(|kind| tuck_emoji::catalog(*kind).entries.len()).collect();
            let usage = load_usage();
            log::info!("catalogs {counts:?} parsed in {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
            proxy.post(AppEvent::CatalogsReady(Box::new(usage)));
            let started = Instant::now();
            let coverage = paths::local_data_dir()
                .and_then(|dir| Coverage::load_or_compute(&dir.join("coverage.json")))
                .unwrap_or_else(|error| {
                    log::warn!("emoji coverage unavailable, showing every emoji: {error:#}");
                    Coverage::all_supported()
                });
            log::info!(
                "emoji coverage: {} supported ({:.0} ms)",
                coverage.supported_count(),
                started.elapsed().as_secs_f64() * 1000.0
            );
            proxy.post(AppEvent::CoverageReady(coverage));
        });
        self.track_worker(handle);
    }

    fn tray_dpi(&self) -> u32 {
        tuck_ui::win::dpi_for_window(self.state.borrow().owner)
    }

    fn show_tray(&self, app: &App) -> bool {
        let dpi = self.tray_dpi();
        let gfx = app.gfx();
        let shown = self.state.borrow_mut().tray.show(&gfx, dpi);
        shown.map_err(|error| log::warn!("tray icon: {error:#}")).is_ok()
    }

    /// Explorer may not be ready at logon: retry every 2 s for up to 2 minutes (TaskbarCreated also re-adds it).
    fn show_tray_with_retry(&self, app: &App, attempts_left: u32) {
        if self.show_tray(app) || attempts_left == 0 {
            return;
        }
        let tuck = self.clone();
        app.set_timer(TRAY_RETRY, move |app| {
            if !tuck.state.borrow().tray.is_shown() {
                tuck.show_tray_with_retry(app, attempts_left - 1);
            }
        });
    }

    fn refresh_tray(&self, app: &App) {
        let dpi = self.tray_dpi();
        let gfx = app.gfx();
        if let Err(error) = self.state.borrow_mut().tray.refresh(&gfx, dpi) {
            log::warn!("tray icon refresh: {error:#}");
        }
    }

    fn run_command(&self, app: &App, command: Command, hook_ok: bool) {
        log::info!("command {command:?}");
        match command {
            Command::Start(Greeting::Silent) => {}
            Command::Start(greeting) => {
                let title = if greeting == Greeting::Installed { "Tuck is installed" } else { "Tuck is running" };
                let detail = if hook_ok {
                    "Win + V for clipboard history, Win + . for emoji"
                } else {
                    "Use the tray icon to open Tuck"
                };
                self.toast(app, Toast::new(ToastIcon::App, title, Some(detail)));
            }
            Command::Clipboard => self.toggle_panel(app, Tab::Clipboard, Opener::Command, Instant::now()),
            Command::Emoji => self.toggle_panel(app, Tab::Emoji, Opener::Command, Instant::now()),
            Command::Settings => self.open_settings(app),
            Command::Quit => self.quit(),
            Command::Uninstall { purge: false } => self.uninstall(app),
            other => log::info!("ignoring {other:?} in the resident instance"),
        }
    }

    // ---- events -----------------------------------------------------------------------------------------------

    fn on_event(&self, app: &App, event: AppEvent) {
        match event {
            AppEvent::Host(message) => self.on_host(app, message),
            AppEvent::Hook(event, at) => self.on_hook(app, event, at),
            AppEvent::Copied { captured, source } => self.on_copied(app, *captured, source),
            AppEvent::Written { result, after } => self.on_written(app, result, after),
            AppEvent::CatalogsReady(usage) => {
                {
                    let mut state = self.state.borrow_mut();
                    state.usage = *usage;
                    state.catalogs_ready = true;
                }
                self.rebuild_pickers(app);
            }
            AppEvent::CoverageReady(coverage) => {
                self.state.borrow_mut().coverage = coverage;
                if self.state.borrow().session.is_some() {
                    self.state.borrow_mut().pickers_stale = true;
                } else {
                    self.rebuild_pickers(app);
                }
            }
            AppEvent::AppIcon(exe, image) => {
                self.state.borrow_mut().rows.set_icon(exe, image.map(Bitmap::new));
                self.refresh_rows(app);
            }
            AppEvent::TextRecognized(result) => self.on_recognized(app, result),
            AppEvent::Imported(result) => self.on_imported(app, result),
            AppEvent::InjectionFailed(message) => {
                let icon = ToastIcon::Symbol(tuck_ui::Icon::Keyboard, Tint::Destructive);
                self.toast(app, Toast::new(icon, "Couldn't type into the app", first_line(&message, 80).as_deref()));
            }
            AppEvent::GlintFailed(error) => self.error_toast(app, "Couldn't open Glint", &error),
            AppEvent::Foreground(window) => self.on_foreground(app, window),
        }
    }

    fn on_host(&self, app: &App, message: HostMessage) {
        match message {
            HostMessage::Forwarded(args) => {
                let parsed = cli::parse(&args);
                for ignored in &parsed.ignored {
                    log::info!("ignoring forwarded argument {ignored:?}");
                }
                let hook_ok = self.state.borrow().hook.is_some();
                self.run_command(app, parsed.command, hook_ok);
            }
            HostMessage::TrayActivate => self.toggle_panel(app, Tab::Clipboard, Opener::Tray, Instant::now()),
            HostMessage::TrayMenu(at) => self.tray_menu(app, at),
            HostMessage::TaskbarCreated => {
                self.show_tray(app);
            }
            HostMessage::SessionUnlocked => {
                if let Some(hook) = &self.state.borrow().hook {
                    hook.on_session_unlock();
                }
            }
            HostMessage::ThemeChanged | HostMessage::DisplayChanged => self.refresh_tray(app),
        }
    }

    fn tray_menu(&self, app: &App, at: PointI) {
        let (dark, paused, owner) = {
            let state = self.state.borrow();
            let dark = match state.settings.theme {
                tuck_core::ThemeMode::Dark => true,
                tuck_core::ThemeMode::Light => false,
                tuck_core::ThemeMode::System => app.system_prefers_dark(),
            };
            (dark, !state.settings.history.enabled, state.owner)
        };
        tuck_sys::tray::set_dark_menus(dark);
        let picked = tuck_sys::tray::show_context_menu(hwnd(owner), &menu_items(paused), at).and_then(command_for);
        match picked {
            Some(TrayCommand::Clipboard) => self.open_panel(app, Tab::Clipboard, Opener::Tray, Instant::now()),
            Some(TrayCommand::Emoji) => self.open_panel(app, Tab::Emoji, Opener::Tray, Instant::now()),
            Some(TrayCommand::TogglePause) => self.set_paused(app, !paused),
            Some(TrayCommand::Settings) => self.open_settings(app),
            Some(TrayCommand::Quit) => self.quit(),
            None => {}
        }
    }

    fn on_hook(&self, app: &App, event: HookEvent, at: Instant) {
        match event {
            HookEvent::Hotkey(hotkey) => {
                log::info!(
                    "hotkey {hotkey:?} reached the UI thread after {:.1} ms",
                    at.elapsed().as_secs_f64() * 1000.0
                );
                let tab = match hotkey {
                    Hotkey::Clipboard => Tab::Clipboard,
                    Hotkey::Emoji => Tab::Emoji,
                };
                self.toggle_panel(app, tab, Opener::Hotkey, at);
            }
            HookEvent::Key(key) => self.on_key(app, key),
            HookEvent::Dismiss(reason) => {
                if self.state.borrow().session.is_some() {
                    log::info!("panel dismissed: {reason:?}");
                    self.close_panel(app);
                }
            }
        }
    }

    /// Routed keys become panel key events plus typed text in the target's keyboard layout.
    fn on_key(&self, app: &App, key: RoutedKey) {
        let text = {
            let mut state = self.state.borrow_mut();
            let Some(layout) = state.session.as_ref().map(|s| s.layout) else { return };
            match key.down.then(|| state.translator.translate(&key, layout)) {
                Some(KeyText::Text(text)) => Some(text),
                _ => None,
            }
        };
        let event = key_event(&key);
        self.with_panel(app, |panel, cx| {
            if key.down {
                panel.event(cx, &Event::KeyDown(event));
                if let Some(text) = text {
                    panel.event(cx, &Event::Text(text));
                }
            } else if event.key == Key::Control {
                panel.event(cx, &Event::KeyUp(event));
            }
        });
    }

    fn on_foreground(&self, app: &App, window: isize) {
        let leaves_target = {
            let state = self.state.borrow();
            state
                .session
                .as_ref()
                .and_then(|s| s.target.as_ref())
                .is_some_and(|target| window != 0 && window != target.hwnd && Some(window) != app.hwnd(self.panel))
        };
        if leaves_target {
            log::info!("panel closed: the foreground moved to another window");
            self.close_panel(app);
        }
    }

    // ---- the panel ----------------------------------------------------------------------------------------------

    /// Same tab while open closes; the other tab switches; closed opens.
    fn toggle_panel(&self, app: &App, tab: Tab, opener: Opener, key_at: Instant) {
        if self.state.borrow().session.is_none() {
            self.open_panel(app, tab, opener, key_at);
            return;
        }
        match self.with_panel(app, |panel, _| panel.tab()) {
            Some(current) if current == tab => self.close_panel(app),
            _ => {
                self.with_panel(app, |panel, cx| panel.select_tab(cx, tab));
            }
        }
    }

    fn open_panel(&self, app: &App, tab: Tab, opener: Opener, key_at: Instant) {
        let started = Instant::now();
        let own_pid = std::process::id();
        let foreground = capture_target().filter(|target| target.pid != own_pid);
        let target = if opener == Opener::Tray { None } else { foreground.clone() };
        let layout = foreground.as_ref().map_or(0, |target| target.layout);
        let cursor = cursor_pos();
        let placement = {
            let state = self.state.borrow();
            match (&target, &state.locator) {
                (Some(target), Some(locator)) => locator.locate(target, CARET_BUDGET),
                (Some(target), None) => target.caret.map(Placement::Caret),
                (None, _) => None,
            }
        };
        let (work, scale) = match (placement, &target) {
            (Some(Placement::Caret(r) | Placement::Element(r)), Some(target)) => {
                (target.work_area, scale_at(PointI::new(r.x, r.y)))
            }
            _ => (work_area_at(cursor), scale_at(cursor)),
        };
        let size_px = ((Panel::SIZE.w * scale).round() as i32, (Panel::SIZE.h * scale).round() as i32);
        let origin = panel_origin(placement, cursor, size_px, work, scale);
        let rect = RectI::new(origin.x, origin.y, size_px.0, size_px.1);
        let how = match (placement, target.as_ref().and_then(|t| t.caret)) {
            (Some(Placement::Caret(_)), Some(_)) => "caret",
            (Some(Placement::Caret(_)), None) => "accessibility caret",
            (Some(Placement::Element(_)), _) => "focused element",
            (None, _) => "cursor",
        };
        self.prepare_for_open(app);
        let options = panel_options(&self.state.borrow().settings, glint_exe().is_some());
        self.with_panel(app, |panel, cx| {
            cx.move_window_px(origin);
            panel.set_options(cx, options);
            panel.open(cx, tab);
            cx.show_window(false);
        });
        let generation = {
            let mut state = self.state.borrow_mut();
            if let Some(hook) = &state.hook {
                hook.begin_capture(rect);
            }
            state.translator.reset();
            state.session_generation += 1;
            let generation = state.session_generation;
            state.session = Some(Session { target, layout, key_at, generation });
            generation
        };
        self.schedule_heartbeat(app, generation);
        log::info!(
            "panel opening on {tab:?} ({opener:?}) at the {how}; placed in {:.1} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }

    /// Unfiltered rows and full picker sections, so the panel opens with everything in place.
    fn prepare_for_open(&self, app: &App) {
        let refilter = !std::mem::take(&mut self.state.borrow_mut().clip_query).is_empty();
        if refilter {
            self.refresh_rows(app);
        }
        if self.state.borrow().pickers_stale {
            self.rebuild_pickers(app);
        } else {
            for kind in Kind::ALL {
                self.show_full_sections(app, kind, false);
            }
        }
    }

    fn close_panel(&self, app: &App) {
        let Some(session) = self.state.borrow_mut().session.take() else { return };
        if let Some(hook) = &self.state.borrow().hook {
            hook.end_capture();
        }
        self.with_panel(app, |panel, cx| panel.close(cx));
        self.schedule_hide(app, session.generation);
    }

    /// Hides the window once the disappear motion finished (unless the panel opened again meanwhile).
    fn schedule_hide(&self, app: &App, generation: u64) {
        let tuck = self.clone();
        app.set_timer(HIDE_POLL, move |app| {
            if tuck.state.borrow().session_generation != generation {
                return;
            }
            let hidden = tuck.with_panel(app, |panel, cx| {
                let done = panel.is_closing_done();
                if done {
                    cx.hide_window();
                }
                done
            });
            match hidden {
                Some(true) => tuck.after_hide(app),
                _ => tuck.schedule_hide(app, generation),
            }
        });
    }

    /// Catches up on what was deferred while the panel was open.
    fn after_hide(&self, app: &App) {
        if self.state.borrow().pickers_stale {
            self.rebuild_pickers(app);
        }
    }

    fn schedule_heartbeat(&self, app: &App, generation: u64) {
        let tuck = self.clone();
        app.set_timer(HEARTBEAT, move |app| tuck.heartbeat(app, generation));
    }

    /// Keeps the hook's failsafe fed and closes when the hook gave up or the target window is gone.
    fn heartbeat(&self, app: &App, generation: u64) {
        let (capturing, target_alive) = {
            let state = self.state.borrow();
            let Some(session) = state.session.as_ref().filter(|s| s.generation == generation) else { return };
            let capturing = state.hook.as_ref().is_none_or(|hook| {
                hook.heartbeat();
                hook.is_capturing()
            });
            (capturing, session.target.as_ref().is_none_or(|target| is_window(target.hwnd)))
        };
        if !capturing || !target_alive {
            log::info!(
                "panel closed: {}",
                if capturing { "the target window closed" } else { "the hook stopped capturing" }
            );
            self.close_panel(app);
            return;
        }
        self.schedule_heartbeat(app, generation);
    }

    fn session_target(&self) -> Option<Target> {
        self.state.borrow().session.as_ref().and_then(|s| s.target.clone())
    }

    fn on_panel(&self, app: &App, action: PanelAction) {
        match action {
            PanelAction::Insert { tab, text, close } => self.insert(app, tab, text, close),
            PanelAction::Paste { id, plain } => {
                let delivery = delivery(self.session_target().as_ref(), &self.state.borrow().settings.paste);
                self.close_panel(app);
                self.deliver_item(app, id, plain, delivery);
            }
            PanelAction::Copy { id } => {
                self.close_panel(app);
                self.deliver_item(app, id, false, Delivery::Copy(CopyReason::Asked));
            }
            PanelAction::SetPinned { id, pinned } => {
                let op = self.state.borrow_mut().history.set_pinned(id, pinned);
                self.save_ops(op.into_iter().collect());
                self.refresh_rows(app);
            }
            PanelAction::Delete { id } => {
                self.remove_items(app, false, |history| {
                    history.remove(id).map(|(item, op)| (vec![item], vec![op])).unwrap_or_default()
                });
            }
            PanelAction::ClearUnpinned => self.remove_items(app, true, History::clear_unpinned),
            PanelAction::CopyImageText { id } => {
                self.close_panel(app);
                self.load_image(id, ImageUse::Ocr);
            }
            PanelAction::EditInGlint { id } => {
                self.close_panel(app);
                self.load_image(id, ImageUse::Glint { id });
            }
            PanelAction::ShowInExplorer { id } => {
                self.close_panel(app);
                self.show_in_explorer(app, id);
            }
            PanelAction::QueryChanged { tab, query } => self.on_query(app, tab, query),
            PanelAction::TabChanged(tab) => {
                if self.with_panel(app, |panel, _| panel.query().is_empty()) == Some(true) {
                    self.on_query(app, tab, String::new());
                }
            }
            PanelAction::NeedThumbnails(ids) => self.provide_thumbnails(app, ids),
            PanelAction::SkinTone(tone) => {
                let mut settings = self.settings();
                settings.emoji.skin_tone = tone;
                self.apply_settings(app, settings);
                self.refresh_settings_window(app);
            }
            PanelAction::SetPaused(paused) => self.set_paused(app, paused),
            PanelAction::OpenSettings => {
                self.close_panel(app);
                self.open_settings(app);
            }
            PanelAction::DragTo(origin) => self.drag_panel(app, origin),
            PanelAction::Close => self.close_panel(app),
        }
    }

    fn drag_panel(&self, app: &App, origin: PointI) {
        let rect = self.with_panel(app, |_, cx| {
            let size = cx.window_rect_px().map(|r| (r.w, r.h));
            cx.move_window_px(origin);
            size
        });
        if let (Some(Some((w, h))), Some(hook)) = (rect, &self.state.borrow().hook) {
            hook.set_panel_rect(RectI::new(origin.x, origin.y, w, h));
        }
    }

    fn on_query(&self, app: &App, tab: Tab, query: String) {
        let Some(kind) = pickers::kind_of(tab) else {
            let changed = {
                let mut state = self.state.borrow_mut();
                let changed = state.clip_query != query;
                state.clip_query = query;
                changed
            };
            if changed {
                self.refresh_rows(app);
            }
            return;
        };
        if query.trim().is_empty() {
            self.show_full_sections(app, kind, false);
            return;
        }
        let sections = {
            let mut state = self.state.borrow_mut();
            if !state.catalogs_ready {
                return;
            }
            state.showing_results[kind_slot(kind)] = true;
            pickers::search_sections(kind, &query, &state.coverage, &state.usage)
        };
        self.with_panel(app, |panel, cx| panel.set_sections(cx, tab, sections));
    }

    /// Gives `kind`'s tab its full sections when it shows search results (or always with `force`).
    fn show_full_sections(&self, app: &App, kind: Kind, force: bool) {
        let sections = {
            let mut state = self.state.borrow_mut();
            let slot = kind_slot(kind);
            if !state.catalogs_ready || !(force || state.showing_results[slot]) {
                return;
            }
            state.showing_results[slot] = false;
            state.full_sections[slot].clone()
        };
        self.with_panel(app, |panel, cx| panel.set_sections(cx, pickers::tab_of(kind), sections));
    }

    fn rebuild_pickers(&self, app: &App) {
        {
            let mut state = self.state.borrow_mut();
            if !state.catalogs_ready {
                return;
            }
            let started = Instant::now();
            let state = &mut *state;
            state.full_sections = Kind::ALL.map(|kind| pickers::sections(kind, &state.coverage, &state.usage));
            state.pickers_stale = false;
            log::debug!("picker sections built in {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
        }
        for kind in Kind::ALL {
            self.show_full_sections(app, kind, true);
        }
    }

    fn insert(&self, app: &App, tab: Tab, text: String, close: bool) {
        let delivery = delivery(self.session_target().as_ref(), &self.state.borrow().settings.paste);
        if close {
            self.close_panel(app);
        }
        if let Some(kind) = pickers::kind_of(tab) {
            let mut state = self.state.borrow_mut();
            let recorded = pickers::base_text(kind, &text).unwrap_or(&text).to_string();
            state.usage.record(kind, &recorded, now_ms());
            state.pickers_stale = true;
        }
        self.schedule_usage_save(app);
        log::info!("insert from {tab:?} ({delivery:?})");
        match delivery {
            Delivery::Paste(_) => {
                if let Some(injector) = &self.state.borrow().injector {
                    injector.send(Injection::Type(text));
                }
            }
            Delivery::Copy(reason) => {
                let payload = WritePayload { text: Some(text), ..WritePayload::default() };
                self.write(app, payload, AfterWrite::Copied { id: None, reason }, None);
            }
        }
    }

    /// Puts history item `id` on the clipboard, then pastes or reports it.
    fn deliver_item(&self, app: &App, id: ClipId, plain: bool, delivery: Delivery) {
        let Some(content) = self.state.borrow().history.get(id).map(|item| item.content.clone()) else { return };
        log::info!("item {id} {:?} ({delivery:?}, plain {plain})", content.kind());
        match content {
            ClipContent::Image { .. } => self.load_image(id, ImageUse::Write { id, delivery }),
            content => self.write_item(app, id, clips::payload(&content, plain, None), delivery),
        }
    }

    fn write_item(&self, app: &App, id: ClipId, payload: WritePayload, delivery: Delivery) {
        match delivery {
            Delivery::Paste(key) => self.write(app, payload, AfterWrite::Pasted(id), Some(key)),
            Delivery::Copy(reason) => self.write(app, payload, AfterWrite::Copied { id: Some(id), reason }, None),
        }
    }

    /// Writes on the clipboard thread; when it is done the paste keys go straight to the injection thread.
    fn write(&self, app: &App, payload: WritePayload, after: AfterWrite, paste: Option<PasteKey>) {
        let (token, injector) = {
            let mut state = self.state.borrow_mut();
            state.write_token += 1;
            (state.write_token, state.injector.clone())
        };
        let proxy = self.proxy.clone();
        let done = move |result: Result<()>| {
            if result.is_ok()
                && let (Some(key), Some(injector)) = (paste, &injector)
            {
                injector.send(Injection::Paste(key));
            }
            proxy.post(AppEvent::Written { result, after });
        };
        let unavailable = {
            let state = self.state.borrow();
            match &state.clipboard {
                Some(clipboard) => {
                    clipboard.write(payload, token, done);
                    false
                }
                None => true,
            }
        };
        if unavailable {
            self.toast(
                app,
                Toast::new(
                    ToastIcon::Symbol(tuck_ui::Icon::Info, Tint::Neutral),
                    "The clipboard isn't ready yet",
                    None,
                ),
            );
        }
    }

    fn on_written(&self, app: &App, result: Result<()>, after: AfterWrite) {
        if let Err(error) = result {
            self.error_toast(app, "Couldn't copy to the clipboard", &error);
            return;
        }
        match after {
            AfterWrite::Pasted(id) => self.touch(app, id),
            AfterWrite::Copied { id, reason } => {
                if let Some(id) = id {
                    self.touch(app, id);
                }
                self.toast(app, copied_toast(reason));
            }
            AfterWrite::Recognized(text) => {
                let line = first_line(&text, 60);
                let hash = tuck_core::clip::text_hash(&text);
                let content = ClipContent::Text { text, html: None, rtf: None };
                self.add_to_history(app, content, hash, None, None, now_ms());
                let icon = ToastIcon::Symbol(tuck_ui::Icon::ScanText, Tint::Accent);
                self.toast(app, Toast::new(icon, "Text copied", line.as_deref()));
            }
        }
    }

    fn touch(&self, app: &App, id: ClipId) {
        let op = self.state.borrow_mut().history.touch(id, now_ms());
        self.save_ops(op.into_iter().collect());
        self.refresh_rows(app);
    }

    fn load_image(&self, id: ClipId, image_use: ImageUse) {
        let blob = match self.state.borrow().history.get(id).map(|item| &item.content) {
            Some(ClipContent::Image { blob, .. }) => blob.clone(),
            _ => return,
        };
        let ticket = {
            let mut state = self.state.borrow_mut();
            state.next_ticket += 1;
            let ticket = state.next_ticket;
            state.pending_images.insert(ticket, image_use);
            ticket
        };
        self.state.borrow().store.send(StoreJob::LoadImage { ticket, blob });
    }

    fn on_image_loaded(&self, app: &App, ticket: u64, result: Result<Image>) {
        let Some(image_use) = self.state.borrow_mut().pending_images.remove(&ticket) else { return };
        let image = match result {
            Ok(image) => image,
            Err(error) => return self.error_toast(app, "Couldn't read the image", &error),
        };
        match image_use {
            ImageUse::Write { id, delivery } => {
                let payload = WritePayload { image: Some(image), ..WritePayload::default() };
                self.write_item(app, id, payload, delivery);
            }
            ImageUse::Ocr => {
                let proxy = self.proxy.clone();
                let handle = spawn_worker("tuck-ocr", move || {
                    proxy.post(AppEvent::TextRecognized(tuck_sys::ocr::recognize(&image)));
                });
                self.track_worker(handle);
            }
            ImageUse::Glint { id } => {
                let proxy = self.proxy.clone();
                let handle = spawn_worker("tuck-glint", move || {
                    if let Err(error) = edit_in_glint(id, &image) {
                        proxy.post(AppEvent::GlintFailed(error));
                    }
                });
                self.track_worker(handle);
            }
        }
    }

    fn on_recognized(&self, app: &App, result: Result<String>) {
        match result {
            Ok(text) if !text.trim().is_empty() => {
                let payload = WritePayload { text: Some(text.clone()), ..WritePayload::default() };
                self.write(app, payload, AfterWrite::Recognized(text), None);
            }
            Ok(_) => {
                let icon = ToastIcon::Symbol(tuck_ui::Icon::ScanText, Tint::Neutral);
                self.toast(app, Toast::new(icon, "No text found", Some("Try an image with larger or sharper text")));
            }
            Err(error) => self.error_toast(app, "Couldn't read text", &error),
        }
    }

    fn show_in_explorer(&self, app: &App, id: ClipId) {
        let first = match self.state.borrow().history.get(id).map(|item| &item.content) {
            Some(ClipContent::Files { paths }) => paths.first().cloned(),
            _ => None,
        };
        if let Some(path) = first
            && let Err(error) = shell::reveal_in_explorer(&path)
        {
            self.error_toast(app, "Couldn't show the file", &error);
        }
    }

    fn provide_thumbnails(&self, app: &App, ids: Vec<ClipId>) {
        let mut ready = Vec::new();
        let mut wanted = Vec::new();
        {
            let state = self.state.borrow();
            for id in ids {
                if let Some(bitmap) = state.rows.thumbnail(id) {
                    ready.push((id, bitmap));
                } else if let Some(ClipContent::Image { blob, .. }) = state.history.get(id).map(|item| &item.content) {
                    wanted.push((id, blob.clone()));
                }
            }
            if !wanted.is_empty() {
                state.store.send(StoreJob::Thumbnails(wanted));
            }
        }
        if !ready.is_empty() {
            self.with_panel(app, |panel, cx| {
                for (id, bitmap) in ready {
                    panel.set_thumbnail(cx, id, bitmap);
                }
            });
        }
    }

    // ---- history ------------------------------------------------------------------------------------------------

    fn on_store(&self, app: &App, event: StoreEvent) {
        match event {
            StoreEvent::Opened(result) => {
                match result {
                    Ok(history) => self.state.borrow_mut().history = history,
                    Err(error) => {
                        self.error_toast(app, "Clipboard history couldn't be opened", &error);
                    }
                }
                self.refresh_rows(app);
                self.start_clipboard();
            }
            StoreEvent::Thumbnail { id, image } => {
                let bitmap = Bitmap::new(image);
                self.state.borrow_mut().rows.set_thumbnail(id, bitmap.clone());
                self.with_panel(app, |panel, cx| panel.set_thumbnail(cx, id, bitmap));
            }
            StoreEvent::ImageLoaded { ticket, result } => self.on_image_loaded(app, ticket, result),
        }
    }

    fn save_ops(&self, ops: Vec<Op>) {
        if !ops.is_empty() {
            self.state.borrow().store.send(StoreJob::Apply(ops));
        }
    }

    fn on_copied(&self, app: &App, captured: Captured, source: Option<SourceApp>) {
        let started = Instant::now();
        let (kind, bytes) = (captured.kind(), captured.byte_len());
        let app_name = source.as_ref().map_or_else(|| "an unknown app".to_string(), |s| s.name.clone());
        let blob = blob_name(self.state.borrow().history.peek_next_id());
        let Some(copied) = clips::copied(captured, blob) else { return };
        let outcome = self.add_to_history(app, copied.content, copied.hash, copied.image, source, now_ms());
        log::info!(
            "copied {kind:?} ({bytes} bytes) from {app_name}: {outcome:?} in {:.1} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }

    /// Adds a copy (an image's pixels go to the store under the blob name `content` carries), trims and saves.
    fn add_to_history(
        &self,
        app: &App,
        content: ClipContent,
        hash: u64,
        image: Option<Image>,
        source: Option<SourceApp>,
        at_ms: i64,
    ) -> AddOutcome {
        let outcome = {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            let blob = match &content {
                ClipContent::Image { blob, .. } => Some(blob.clone()),
                _ => None,
            };
            let bytes = tuck_core::content_bytes(&content);
            let (outcome, mut ops) = state.history.add(content, hash, bytes, source, at_ms);
            if let (AddOutcome::Added(id), Some(blob), Some(image)) = (outcome, blob, image) {
                state.store.send(StoreJob::PutImage { id, blob, image });
            }
            let (removed, trim_ops) = state.history.trim(state.settings.history.max_items);
            for item in &removed {
                state.rows.forget(item.id);
            }
            ops.extend(trim_ops);
            state.store.send(StoreJob::Apply(ops));
            outcome
        };
        self.refresh_rows(app);
        outcome
    }

    /// Removes what `remove` picks, saves it and, after bulk removals, compacts the log.
    fn remove_items(
        &self,
        app: &App,
        compact: bool,
        remove: impl FnOnce(&mut History) -> (Vec<tuck_core::ClipItem>, Vec<Op>),
    ) {
        {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            let (removed, ops) = remove(&mut state.history);
            for item in &removed {
                state.rows.forget(item.id);
            }
            if !ops.is_empty() {
                state.store.send(StoreJob::Apply(ops));
                if compact {
                    state.store.send(StoreJob::Compact(Box::new(state.history.clone())));
                }
            }
        }
        self.refresh_rows(app);
        self.refresh_settings_window(app);
    }

    fn refresh_rows(&self, app: &App) {
        let (rows, total, missing) = {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            let ids = state.history.search(&state.clip_query);
            let (rows, missing) = state.rows.rows(&state.history, &ids);
            (rows, state.history.len(), missing)
        };
        if let Some(icons) = &self.state.borrow().icons {
            for exe in missing {
                icons.request(exe);
            }
        }
        self.with_panel(app, |panel, cx| panel.set_clips(cx, rows, total));
    }

    fn on_imported(&self, app: &App, result: Result<Vec<Captured>>) {
        let items = match result {
            Ok(items) => items,
            Err(error) => {
                log::warn!("importing Windows clipboard history: {error:#}");
                self.state.borrow_mut().import = ImportState::Failed(error_text(&error));
                self.refresh_settings_window(app);
                return;
            }
        };
        let now = now_ms();
        let count = items.len() as i64;
        let mut added = 0;
        for (index, captured) in items.into_iter().enumerate() {
            let blob = blob_name(self.state.borrow().history.peek_next_id());
            let Some(copied) = clips::copied(captured, blob) else { continue };
            let at = now - (count - index as i64);
            if let AddOutcome::Added(_) = self.add_to_history(app, copied.content, copied.hash, copied.image, None, at)
            {
                added += 1;
            }
        }
        log::info!("imported {added} of {count} Windows clipboard history items");
        self.state.borrow_mut().import = ImportState::Done(added);
        self.refresh_settings_window(app);
    }

    // ---- settings -----------------------------------------------------------------------------------------------

    fn set_paused(&self, app: &App, paused: bool) {
        let mut settings = self.settings();
        settings.history.enabled = !paused;
        self.apply_settings(app, settings);
        self.refresh_settings_window(app);
    }

    fn settings_env(&self) -> SettingsEnv {
        let state = self.state.borrow();
        SettingsEnv {
            version: crate::VERSION.to_string(),
            install: match install::installed_exe_path() {
                Ok(path) if install::is_installed() => InstallState::Installed { path: path.display().to_string() },
                _ => InstallState::NotInstalled,
            },
            history_items: state.history.len(),
            import: state.import.clone(),
        }
    }

    fn open_settings(&self, app: &App) {
        if let Some(window) = self.state.borrow().settings_window {
            app.with_view(window, |_: &mut SettingsView, cx| cx.activate());
            return;
        }
        let view = SettingsView::new(self.settings(), self.settings_env());
        let spec = WindowSpec::normal("Tuck Settings", WINDOW_SIZE)
            .unified_title_bar()
            .min_size(MIN_SIZE)
            .centered_in(work_area_at(cursor_pos()));
        match app.open(spec, view) {
            Ok(window) => self.state.borrow_mut().settings_window = Some(window),
            Err(error) => self.error_toast(app, "Couldn't open settings", &error),
        }
    }

    fn refresh_settings_window(&self, app: &App) {
        let Some(window) = self.state.borrow().settings_window else { return };
        let settings = self.settings();
        let env = self.settings_env();
        app.with_view(window, |view: &mut SettingsView, cx| {
            view.set_settings(cx, settings);
            view.set_env(cx, env);
        });
    }

    fn on_settings(&self, app: &App, message: SettingsMessage) {
        match message {
            SettingsMessage::Changed(new) => self.apply_settings(app, *new),
            SettingsMessage::Request(request) => self.on_settings_request(app, request),
            SettingsMessage::Closed => self.state.borrow_mut().settings_window = None,
        }
    }

    fn apply_settings(&self, app: &App, new: Settings) {
        let (changes, hook_missing) = {
            let mut state = self.state.borrow_mut();
            let changes = effects(&state.settings, &new);
            state.settings = new.clone();
            if changes.shortcuts
                && let Some(hook) = &state.hook
            {
                hook.set_config(HookConfig::from(&new.shortcuts));
            }
            if changes.capture_filter
                && let Some(clipboard) = &state.clipboard
            {
                clipboard.set_filter(CaptureFilter::from(&new.history));
            }
            (changes, state.hook.is_none())
        };
        if changes.shortcuts && hook_missing && !self.start_hook() {
            let icon = ToastIcon::Symbol(tuck_ui::Icon::Keyboard, Tint::Destructive);
            self.toast(app, Toast::new(icon, "Shortcuts are unavailable", Some("Use the tray icon to open Tuck")));
        }
        if let Some(max) = changes.trim_to {
            self.remove_items(app, true, |history| history.trim(max));
        }
        if changes.appearance {
            app.set_appearance(new.theme, false);
        }
        if let Some(enabled) = changes.launch_at_login
            && let Err(error) = install::set_launch_at_login(enabled, &exe_for_login())
        {
            self.state.borrow_mut().settings.launch_at_login = !enabled;
            self.refresh_settings_window(app);
            self.error_toast(app, "Couldn't change launch at login", &error);
        }
        if changes.panel_options {
            let options = panel_options(&new, glint_exe().is_some());
            self.with_panel(app, |panel, cx| panel.set_options(cx, options));
        }
        self.schedule_save(app);
    }

    fn on_settings_request(&self, app: &App, request: SettingsRequest) {
        match request {
            SettingsRequest::AddApp => {
                let apps = running_apps();
                if let Some(window) = self.state.borrow().settings_window {
                    app.with_view(window, |view: &mut SettingsView, cx| view.open_app_menu(cx, apps));
                }
            }
            SettingsRequest::Import => self.import_windows_history(app),
            SettingsRequest::ClearHistory => {
                self.remove_items(app, true, |history| {
                    let ids: Vec<ClipId> = history.items().iter().map(|item| item.id).collect();
                    ids.into_iter().filter_map(|id| history.remove(id)).unzip()
                });
                let icon = ToastIcon::Symbol(tuck_ui::Icon::Trash, Tint::Neutral);
                self.toast(app, Toast::new(icon, "Clipboard history cleared", None));
            }
            SettingsRequest::ClearFrequent => {
                {
                    let mut state = self.state.borrow_mut();
                    for kind in Kind::ALL {
                        state.usage.clear(kind);
                    }
                    state.pickers_stale = true;
                }
                self.save_usage_now();
                if self.state.borrow().session.is_none() {
                    self.rebuild_pickers(app);
                }
            }
            SettingsRequest::OpenGitHub => {
                if let Err(error) = shell::open_uri(GITHUB_URL) {
                    self.error_toast(app, "Couldn't open the browser", &error);
                }
            }
            SettingsRequest::Install => self.install_from_settings(app),
            SettingsRequest::Uninstall => self.uninstall(app),
        }
    }

    /// Reads Windows' own clipboard history on a worker while the settings window is in front (WinRT refuses
    /// background callers), then adds it oldest first.
    fn import_windows_history(&self, app: &App) {
        if self.state.borrow().import == ImportState::Running {
            return;
        }
        self.state.borrow_mut().import = ImportState::Running;
        self.refresh_settings_window(app);
        let proxy = self.proxy.clone();
        let handle = spawn_worker("tuck-import", move || {
            proxy.post(AppEvent::Imported(tuck_clip::import_windows_history()));
        });
        if handle.is_none() {
            self.state.borrow_mut().import = ImportState::Failed("Couldn't start the import".into());
            self.refresh_settings_window(app);
        }
        self.track_worker(handle);
    }

    fn install_from_settings(&self, app: &App) {
        self.save_now();
        let result = std::env::current_exe()
            .context("locating tuck.exe")
            .and_then(|exe| install::install(&exe, &self.settings()).map(|target| (exe, target)));
        match result {
            Ok((exe, target)) if !crate::same_file(&exe, &target) => {
                log::info!("installed to {}; handing over", target.display());
                self.state.borrow_mut().relaunch = Some(target);
                self.quit();
            }
            Ok(_) => {
                self.refresh_settings_window(app);
                self.toast(
                    app,
                    Toast::new(
                        ToastIcon::App,
                        "Tuck is installed",
                        Some("It starts with Windows and lives in the tray"),
                    ),
                );
            }
            Err(error) => self.error_toast(app, "Couldn't install Tuck", &error),
        }
    }

    /// Stops the hook, tray and clipboard watch, removes Tuck's registrations (history and settings stay) and exits.
    fn uninstall(&self, app: &App) {
        self.close_panel(app);
        {
            let mut state = self.state.borrow_mut();
            state.hook = None;
            state.clipboard = None;
            state.tray.remove();
        }
        match install::uninstall(false) {
            Ok(()) => {
                log::info!("uninstalled");
                self.quit();
            }
            Err(error) => {
                self.error_toast(app, "Couldn't uninstall Tuck", &error);
                self.start_hook();
                self.start_clipboard();
                self.show_tray(app);
            }
        }
    }

    fn schedule_save(&self, app: &App) {
        if let Some(timer) = self.state.borrow_mut().save_timer.take() {
            app.cancel_timer(timer);
        }
        let tuck = self.clone();
        let timer = app.set_timer(SAVE_DEBOUNCE, move |_| tuck.save_now());
        self.state.borrow_mut().save_timer = Some(timer);
    }

    fn save_now(&self) {
        let settings = {
            let mut state = self.state.borrow_mut();
            state.save_timer = None;
            state.settings.clone()
        };
        if let Err(error) = settings_store::save_settings(&settings) {
            log::error!("saving settings: {error:#}");
        }
    }

    fn schedule_usage_save(&self, app: &App) {
        if let Some(timer) = self.state.borrow_mut().usage_timer.take() {
            app.cancel_timer(timer);
        }
        let tuck = self.clone();
        let timer = app.set_timer(USAGE_SAVE_DEBOUNCE, move |_| tuck.save_usage_now());
        self.state.borrow_mut().usage_timer = Some(timer);
    }

    fn save_usage_now(&self) {
        let usage = {
            let mut state = self.state.borrow_mut();
            state.usage_timer = None;
            state.usage.clone()
        };
        if let Err(error) = save_usage(&usage) {
            log::warn!("saving usage: {error:#}");
        }
    }

    // ---- toasts -----------------------------------------------------------------------------------------------

    fn error_toast(&self, app: &App, title: &str, error: &anyhow::Error) {
        log::error!("{title}: {error:#}");
        let icon = ToastIcon::Symbol(tuck_ui::Icon::Info, Tint::Destructive);
        self.toast(app, Toast::new(icon, title, Some(&error_text(error))));
    }

    fn toast(&self, app: &App, toast: Toast) {
        let cursor = cursor_pos();
        let card = crate::toast::card_size(&app.gfx(), &toast);
        let layout = corner_layout(card, work_area_at(cursor), scale_at(cursor), 0);
        let (id, previous) = {
            let mut state = self.state.borrow_mut();
            state.next_toast_id += 1;
            (state.next_toast_id, state.toast.take())
        };
        if let Some((_, window)) = previous {
            app.close(window);
        }
        let view = ToastView::new(id, toast, layout.card);
        match app.open(WindowSpec::popup(layout.origin_px, layout.size), view) {
            Ok(window) => self.state.borrow_mut().toast = Some((id, window)),
            Err(error) => log::error!("toast window: {error:#}"),
        }
    }

    // ---- quit -------------------------------------------------------------------------------------------------

    /// Saves settings and usage, stops the hook, clipboard watch and tray, lets the store finish its queue (bounded)
    /// and leaves the loop.
    fn quit(&self) {
        self.save_now();
        self.save_usage_now();
        let workers = {
            let mut state = self.state.borrow_mut();
            state.session = None;
            state.hook = None;
            state.clipboard = None;
            state.foreground = None;
            state.locator = None;
            state.injector = None;
            state.icons = None;
            state.tray.remove();
            let mut workers = std::mem::take(&mut state.workers);
            workers.extend(state.store.finish());
            workers
        };
        join_bounded(workers, QUIT_WORKER_WAIT);
        self.state.borrow_mut().host = None;
        self.proxy.post(QuitRequest);
    }

    /// WM_ENDSESSION: Windows is logging off or shutting down and will end the process when this returns.
    fn end_session(&self) {
        log::info!("session ending");
        if self.state.try_borrow_mut().is_err() {
            return;
        }
        self.save_now();
        self.save_usage_now();
        let workers = {
            let mut state = self.state.borrow_mut();
            let mut workers = std::mem::take(&mut state.workers);
            workers.extend(state.store.finish());
            workers
        };
        join_bounded(workers, END_SESSION_WORKER_WAIT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuck_sys::Mods;

    fn target(exe: &str, elevated: bool) -> Target {
        Target {
            hwnd: 1,
            focus: 1,
            thread_id: 1,
            pid: 1,
            exe: Some(PathBuf::from(exe)),
            layout: 0,
            caret: None,
            work_area: RectI::new(0, 0, 1920, 1040),
            elevated,
        }
    }

    #[test]
    fn delivery_pastes_with_the_app_key_unless_it_cannot() {
        let paste = PasteSettings::default();
        assert_eq!(delivery(Some(&target(r"C:\Windows\notepad.exe", false)), &paste), Delivery::Paste(PasteKey::CtrlV));
        assert_eq!(
            delivery(Some(&target(r"C:\Program Files\PuTTY\PUTTY.EXE", false)), &paste),
            Delivery::Paste(PasteKey::ShiftInsert)
        );
        assert_eq!(
            delivery(Some(&target(r"C:\Windows\regedit.exe", true)), &paste),
            Delivery::Copy(CopyReason::Elevated)
        );
        assert_eq!(delivery(None, &paste), Delivery::Copy(CopyReason::NoTarget));
    }

    #[test]
    fn routed_keys_become_panel_keys() {
        let key = RoutedKey {
            vk: 0x31,
            scan: 2,
            extended: false,
            down: true,
            repeat: false,
            mods: Mods { ctrl: true, ..Mods::default() },
            caps_lock: false,
        };
        let event = key_event(&key);
        assert_eq!((event.key, event.mods), (Key::Char('1'), Modifiers::CTRL));
        let altgr = RoutedKey { vk: 0x45, mods: Mods { altgr: true, ..Mods::default() }, ..key };
        assert_eq!(key_event(&altgr).mods, Modifiers::NONE);
        let control = RoutedKey { vk: 0xA2, down: false, ..key };
        assert_eq!(key_event(&control).key, Key::Control);
    }

    #[test]
    fn toast_lines_are_short_and_never_empty() {
        assert_eq!(first_line("\n  hello world  \nsecond", 5).as_deref(), Some("hello…"));
        assert_eq!(first_line("  \n ", 5), None);
        assert_eq!(copied_toast(CopyReason::Asked).detail, None);
        assert!(copied_toast(CopyReason::Elevated).detail.is_some_and(|d| d.contains("administrator")));
    }
}
