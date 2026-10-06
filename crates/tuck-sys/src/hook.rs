use anyhow::{Context, Result};
use std::{
    cell::RefCell,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};
use tuck_core::{PointI, RectI, settings::ShortcutSettings};
use windows::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM},
    System::{
        LibraryLoader::GetModuleHandleW,
        SystemInformation::GetTickCount64,
        Threading::{
            GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY, THREAD_PRIORITY_HIGHEST,
            THREAD_PRIORITY_TIME_CRITICAL,
        },
    },
    UI::{
        Input::KeyboardAndMouse::{GetAsyncKeyState, GetKeyState},
        WindowsAndMessaging::*,
    },
};

/// `dwExtraInfo` of every event Tuck injects ("TUCK"); the hook ignores these.
pub const MARKER: usize = 0x5455_434B;
pub const HEARTBEAT_TIMEOUT_MS: u64 = 1000;
const REINSTALL_INTERVAL_MS: u32 = 600_000;

const REINSTALL: u32 = WM_APP + 1;
const BEGIN_CAPTURE: u32 = WM_APP + 2;
const SET_PANEL_RECT: u32 = WM_APP + 3;
const END_CAPTURE: u32 = WM_APP + 4;
const SYNC_MOUSE_HOOK: u32 = WM_APP + 5;

pub(crate) mod vk {
    pub const BACK: u16 = 0x08;
    pub const TAB: u16 = 0x09;
    pub const RETURN: u16 = 0x0d;
    pub const SHIFT: u16 = 0x10;
    pub const CONTROL: u16 = 0x11;
    pub const MENU: u16 = 0x12;
    pub const CAPITAL: u16 = 0x14;
    pub const IME_FIRST: u16 = 0x15;
    pub const IME_LAST: u16 = 0x1a;
    pub const CONVERT: u16 = 0x1c;
    pub const MODECHANGE: u16 = 0x1f;
    pub const SNAPSHOT: u16 = 0x2c;
    pub const INSERT: u16 = 0x2d;
    pub const KEY_1: u16 = 0x31;
    pub const KEY_9: u16 = 0x39;
    pub const KEY_A: u16 = 0x41;
    pub const KEY_P: u16 = 0x50;
    pub const KEY_V: u16 = 0x56;
    pub const LWIN: u16 = 0x5b;
    pub const RWIN: u16 = 0x5c;
    pub const SLEEP: u16 = 0x5f;
    pub const F1: u16 = 0x70;
    pub const F24: u16 = 0x87;
    pub const NUMLOCK: u16 = 0x90;
    pub const SCROLL: u16 = 0x91;
    pub const LSHIFT: u16 = 0xa0;
    pub const RSHIFT: u16 = 0xa1;
    pub const LCONTROL: u16 = 0xa2;
    pub const RCONTROL: u16 = 0xa3;
    pub const LMENU: u16 = 0xa4;
    pub const RMENU: u16 = 0xa5;
    pub const BROWSER_BACK: u16 = 0xa6;
    pub const LAUNCH_APP2: u16 = 0xb7;
    pub const OEM_1: u16 = 0xba;
    pub const OEM_PERIOD: u16 = 0xbe;
    pub const PROCESSKEY: u16 = 0xe5;
    pub const PACKET: u16 = 0xe7;
    /// Unassigned; injected to stop Win/Alt releases from opening Start or a menu bar.
    pub const MASK: u16 = 0xe8;
    pub const NONE: u16 = 0xff;
}

const KEY_COUNT: usize = 256;
const MODIFIERS: [u16; 11] = [
    vk::LWIN,
    vk::RWIN,
    vk::SHIFT,
    vk::LSHIFT,
    vk::RSHIFT,
    vk::CONTROL,
    vk::LCONTROL,
    vk::RCONTROL,
    vk::MENU,
    vk::LMENU,
    vk::RMENU,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hotkey {
    Clipboard,
    Emoji,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HookConfig {
    pub win_v: bool,
    pub win_period: bool,
}
impl From<&ShortcutSettings> for HookConfig {
    fn from(value: &ShortcutSettings) -> Self {
        Self { win_v: value.win_v, win_period: value.win_period }
    }
}
impl HookConfig {
    fn bits(self) -> u8 {
        self.win_v as u8 | (self.win_period as u8) << 1
    }
    fn from_bits(bits: u8) -> Self {
        Self { win_v: bits & 1 != 0, win_period: bits & 2 != 0 }
    }
}

/// Modifiers as the panel should see them: AltGr is not Ctrl/Alt, the Win key that opened the panel is not Win.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub win: bool,
    pub altgr: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoutedKey {
    pub vk: u16,
    pub scan: u16,
    pub extended: bool,
    pub down: bool,
    pub repeat: bool,
    pub mods: Mods,
    pub caps_lock: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DismissReason {
    ClickOutside,
    Shortcut,
    Stalled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookEvent {
    Hotkey(Hotkey),
    Key(RoutedKey),
    Dismiss(DismissReason),
}

/// One raw keyboard event as the hook sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyInput {
    pub vk: u16,
    pub scan: u16,
    pub extended: bool,
    pub up: bool,
    pub caps_lock: bool,
    pub now_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub swallow: bool,
    pub event: Option<HookEvent>,
    pub inject_mask: bool,
}
impl Decision {
    fn pass_with(event: HookEvent) -> Self {
        Self { event: Some(event), ..Self::default() }
    }
    fn swallow_with(event: Option<HookEvent>) -> Self {
        Self { swallow: true, event, ..Self::default() }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Fate {
    #[default]
    Passed,
    Swallowed,
    Routed,
}

enum KeyClass {
    Modifier,
    Quiet,
    Function,
    Other,
}

fn classify(vk: u16) -> KeyClass {
    match vk {
        vk::SHIFT | vk::CONTROL | vk::MENU | vk::LWIN | vk::RWIN | vk::LSHIFT..=vk::RMENU => KeyClass::Modifier,
        vk::CAPITAL | vk::NUMLOCK | vk::SCROLL => KeyClass::Quiet,
        vk::BROWSER_BACK..=vk::LAUNCH_APP2 | vk::SLEEP | vk::SNAPSHOT => KeyClass::Quiet,
        vk::IME_FIRST..=vk::IME_LAST | vk::CONVERT..=vk::MODECHANGE | vk::PROCESSKEY => KeyClass::Quiet,
        vk::MASK | vk::NONE => KeyClass::Quiet,
        vk::F1..=vk::F24 => KeyClass::Function,
        _ => KeyClass::Other,
    }
}

fn is_panel_shortcut(vk: u16, mods: Mods) -> bool {
    mods.ctrl
        && !mods.alt
        && !mods.win
        && match vk {
            vk::TAB => true,
            vk::BACK | vk::RETURN | vk::KEY_A | vk::KEY_P | vk::KEY_1..=vk::KEY_9 => !mods.shift,
            _ => false,
        }
}

fn is_win(vk: u16) -> bool {
    vk == vk::LWIN || vk == vk::RWIN
}

struct Capture {
    panel: RectI,
    /// Left/right Win held when capture began; while still held they count as up.
    opening_win: [bool; 2],
    last_heartbeat_ms: u64,
}

/// The pure routing state machine behind `InputHook` (DESIGN §3). Time is passed in, so tests drive the failsafe.
pub struct HookState {
    pub config: HookConfig,
    down: [bool; KEY_COUNT],
    fates: [Fate; KEY_COUNT],
    capture: Option<Capture>,
}
impl HookState {
    pub fn new(config: HookConfig) -> Self {
        Self { config, down: [false; KEY_COUNT], fates: [Fate::Passed; KEY_COUNT], capture: None }
    }

    pub fn is_capturing(&self) -> bool {
        self.capture.is_some()
    }

    pub fn begin_capture(&mut self, panel: RectI, now_ms: u64) {
        let opening_win = [self.is_down(vk::LWIN), self.is_down(vk::RWIN)];
        self.capture = Some(Capture { panel, opening_win, last_heartbeat_ms: now_ms });
    }

    pub fn set_panel_rect(&mut self, panel: RectI) {
        if let Some(capture) = &mut self.capture {
            capture.panel = panel;
        }
    }

    pub fn end_capture(&mut self) {
        self.capture = None;
    }

    pub fn heartbeat(&mut self, at_ms: u64) {
        if let Some(capture) = &mut self.capture {
            capture.last_heartbeat_ms = capture.last_heartbeat_ms.max(at_ms);
        }
    }

    /// Clears keys we think are down although they are physically up (their key-up was missed while the hook was
    /// removed or an elevated window had focus). Only keys that reached the system have a trustworthy async state.
    pub fn release_stale_keys(&mut self, current_vk: u16, current_up: bool, is_down: impl Fn(u16) -> bool) {
        for vk in MODIFIERS {
            if vk != current_vk && self.is_down(vk) && !is_down(vk) {
                self.release(vk);
            }
        }
        let index = current_vk as usize;
        if index < KEY_COUNT
            && !current_up
            && self.down[index]
            && self.fates[index] == Fate::Passed
            && !is_down(current_vk)
        {
            self.release(current_vk);
        }
    }

    /// After a hook reinstall: forget everything except the modifiers that are physically down.
    pub fn reset(&mut self, is_down: impl Fn(u16) -> bool) {
        self.down = [false; KEY_COUNT];
        self.fates = [Fate::Passed; KEY_COUNT];
        for vk in MODIFIERS {
            self.down[vk as usize] = is_down(vk);
        }
        let win_down = [self.is_down(vk::LWIN), self.is_down(vk::RWIN)];
        if let Some(capture) = &mut self.capture {
            capture.opening_win[0] &= win_down[0];
            capture.opening_win[1] &= win_down[1];
        }
    }

    /// The event for `vk` could not be delivered: let the rest of its press through.
    pub fn forget(&mut self, vk: u16) {
        if let Some(fate) = self.fates.get_mut(vk as usize) {
            *fate = Fate::Passed;
        }
    }

    pub fn on_key(&mut self, key: KeyInput) -> Decision {
        let index = key.vk as usize;
        if index >= KEY_COUNT {
            return Decision::default();
        }
        let repeat = !key.up && self.down[index];
        self.down[index] = !key.up;
        if key.up && is_win(key.vk) {
            self.clear_opening_win(key.vk);
        }
        let stalled = self.end_stalled_capture(key.now_ms);
        let mut decision = if key.up {
            self.key_up(key)
        } else if repeat {
            self.key_repeat(key)
        } else if stalled {
            self.fates[index] = Fate::Passed;
            Decision::default()
        } else {
            self.key_down(key)
        };
        if stalled {
            decision.event = Some(HookEvent::Dismiss(DismissReason::Stalled));
        }
        decision
    }

    pub fn on_button_down(&mut self, at: PointI, now_ms: u64) -> Decision {
        if self.end_stalled_capture(now_ms) {
            return Decision::pass_with(HookEvent::Dismiss(DismissReason::Stalled));
        }
        match &self.capture {
            Some(capture) if !capture.panel.contains(at) => {
                Decision::pass_with(HookEvent::Dismiss(DismissReason::ClickOutside))
            }
            _ => Decision::default(),
        }
    }

    fn key_up(&mut self, key: KeyInput) -> Decision {
        match std::mem::take(&mut self.fates[key.vk as usize]) {
            Fate::Swallowed => Decision::swallow_with(None),
            Fate::Routed => Decision::swallow_with(self.capture.is_some().then(|| self.routed(key, false))),
            Fate::Passed if self.capture.is_some() && matches!(classify(key.vk), KeyClass::Modifier) => {
                Decision::pass_with(self.routed(key, false))
            }
            Fate::Passed => Decision::default(),
        }
    }

    fn key_repeat(&mut self, key: KeyInput) -> Decision {
        match self.fates[key.vk as usize] {
            Fate::Swallowed => Decision::swallow_with(None),
            Fate::Routed => Decision::swallow_with(self.capture.is_some().then(|| self.routed(key, true))),
            Fate::Passed => Decision::default(),
        }
    }

    fn key_down(&mut self, key: KeyInput) -> Decision {
        let index = key.vk as usize;
        self.fates[index] = Fate::Passed;
        if let Some(hotkey) = self.hotkey(key.vk) {
            self.fates[index] = Fate::Swallowed;
            return Decision { swallow: true, event: Some(HookEvent::Hotkey(hotkey)), inject_mask: true };
        }
        if self.capture.is_none() {
            return Decision::default();
        }
        let mods = self.mods();
        match classify(key.vk) {
            KeyClass::Modifier => Decision::pass_with(self.routed(key, false)),
            KeyClass::Quiet => Decision::default(),
            KeyClass::Function => Decision::pass_with(HookEvent::Dismiss(DismissReason::Shortcut)),
            KeyClass::Other if (mods.ctrl || mods.alt || mods.win) && !is_panel_shortcut(key.vk, mods) => {
                Decision::pass_with(HookEvent::Dismiss(DismissReason::Shortcut))
            }
            KeyClass::Other => {
                self.fates[index] = Fate::Routed;
                Decision {
                    swallow: true,
                    event: Some(self.routed(key, false)),
                    inject_mask: self.any_down(&[vk::LWIN, vk::RWIN, vk::MENU, vk::LMENU, vk::RMENU]),
                }
            }
        }
    }

    fn hotkey(&self, vk: u16) -> Option<Hotkey> {
        let win_alone = self.effective_win()
            && !self.any_down(&[vk::SHIFT, vk::LSHIFT, vk::RSHIFT])
            && !self.any_down(&[vk::CONTROL, vk::LCONTROL, vk::RCONTROL])
            && !self.any_down(&[vk::MENU, vk::LMENU, vk::RMENU]);
        if !win_alone {
            return None;
        }
        match vk {
            vk::KEY_V if self.config.win_v => Some(Hotkey::Clipboard),
            vk::OEM_PERIOD | vk::OEM_1 if self.config.win_period => Some(Hotkey::Emoji),
            _ => None,
        }
    }

    fn routed(&self, key: KeyInput, repeat: bool) -> HookEvent {
        HookEvent::Key(RoutedKey {
            vk: key.vk,
            scan: key.scan,
            extended: key.extended,
            down: !key.up,
            repeat,
            mods: self.mods(),
            caps_lock: key.caps_lock,
        })
    }

    fn mods(&self) -> Mods {
        let altgr = self.is_down(vk::RMENU);
        Mods {
            shift: self.any_down(&[vk::SHIFT, vk::LSHIFT, vk::RSHIFT]),
            ctrl: self.is_down(vk::RCONTROL) || (!altgr && self.any_down(&[vk::CONTROL, vk::LCONTROL])),
            alt: self.any_down(&[vk::MENU, vk::LMENU]),
            win: self.effective_win(),
            altgr,
        }
    }

    fn effective_win(&self) -> bool {
        let opening = self.capture.as_ref().map_or([false; 2], |capture| capture.opening_win);
        (self.is_down(vk::LWIN) && !opening[0]) || (self.is_down(vk::RWIN) && !opening[1])
    }

    fn end_stalled_capture(&mut self, now_ms: u64) -> bool {
        let stalled = self
            .capture
            .as_ref()
            .is_some_and(|capture| now_ms.saturating_sub(capture.last_heartbeat_ms) > HEARTBEAT_TIMEOUT_MS);
        if stalled {
            self.capture = None;
        }
        stalled
    }

    fn release(&mut self, vk: u16) {
        self.down[vk as usize] = false;
        if is_win(vk) {
            self.clear_opening_win(vk);
        }
    }

    fn clear_opening_win(&mut self, vk: u16) {
        if let Some(capture) = &mut self.capture {
            capture.opening_win[usize::from(vk == vk::RWIN)] = false;
        }
    }

    fn is_down(&self, vk: u16) -> bool {
        self.down[vk as usize]
    }

    fn any_down(&self, keys: &[u16]) -> bool {
        keys.iter().any(|&vk| self.is_down(vk))
    }
}

struct Shared {
    config: AtomicU8,
    capturing: AtomicBool,
    last_heartbeat_ms: AtomicU64,
    panel_rect: Mutex<RectI>,
}
impl Shared {
    fn panel_rect(&self) -> RectI {
        *self.panel_rect.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    fn set_panel_rect(&self, rect: RectI) {
        *self.panel_rect.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = rect;
    }
}

struct HookContext {
    state: HookState,
    shared: Arc<Shared>,
    events: mpsc::Sender<HookEvent>,
    thread_id: u32,
}
impl HookContext {
    fn sync_from_shared(&mut self) {
        self.state.config = HookConfig::from_bits(self.shared.config.load(Ordering::Relaxed));
        self.state.heartbeat(self.shared.last_heartbeat_ms.load(Ordering::Relaxed));
    }

    fn on_key(&mut self, event: &KBDLLHOOKSTRUCT) -> Decision {
        self.sync_from_shared();
        let key = KeyInput {
            vk: event.vkCode as u16,
            scan: event.scanCode as u16,
            extended: event.flags.contains(LLKHF_EXTENDED),
            up: event.flags.contains(LLKHF_UP),
            caps_lock: caps_lock_on(),
            now_ms: now_ms(),
        };
        self.state.release_stale_keys(key.vk, key.up, key_is_down);
        let was_capturing = self.state.is_capturing();
        let decision = self.state.on_key(key);
        self.after_decision(was_capturing);
        if let Some(event) = decision.event
            && self.events.send(event).is_err()
        {
            self.state.forget(key.vk);
            return Decision::default();
        }
        decision
    }

    fn on_button_down(&mut self, event: &MSLLHOOKSTRUCT) {
        self.sync_from_shared();
        let was_capturing = self.state.is_capturing();
        let decision = self.state.on_button_down(PointI::new(event.pt.x, event.pt.y), now_ms());
        self.after_decision(was_capturing);
        if let Some(event) = decision.event {
            let _ = self.events.send(event);
        }
    }

    fn after_decision(&self, was_capturing: bool) {
        if was_capturing && !self.state.is_capturing() {
            self.shared.capturing.store(false, Ordering::Relaxed);
            post(self.thread_id, SYNC_MOUSE_HOOK);
        }
    }
}
thread_local! { static CONTEXT: RefCell<Option<HookContext>> = const { RefCell::new(None) }; }

fn now_ms() -> u64 {
    unsafe { GetTickCount64() }
}

pub(crate) fn key_is_down(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
}

fn caps_lock_on() -> bool {
    unsafe { GetKeyState(vk::CAPITAL as i32) & 1 != 0 }
}

fn post(thread_id: u32, message: u32) {
    unsafe {
        let _ = PostThreadMessageW(thread_id, message, WPARAM(0), LPARAM(0));
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if event.dwExtraInfo != MARKER {
            let decision = CONTEXT.with(|context| {
                context.borrow_mut().as_mut().map_or_else(Decision::default, |context| context.on_key(event))
            });
            if decision.inject_mask {
                crate::inject::send_mask();
            }
            if decision.swallow {
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let button_down = matches!(wparam.0 as u32, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN);
    if code == HC_ACTION as i32 && button_down {
        let event = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if event.dwExtraInfo != MARKER {
            CONTEXT.with(|context| {
                if let Some(context) = context.borrow_mut().as_mut() {
                    context.on_button_down(event);
                }
            });
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Keeps the hook responsive while a game saturates the CPU.
fn raise_current_thread_priority(priority: THREAD_PRIORITY) {
    if let Err(error) = unsafe { SetThreadPriority(GetCurrentThread(), priority) } {
        log::warn!("Could not raise hook thread priority: {error}");
    }
}

fn install_hook(id: WINDOWS_HOOK_ID, procedure: HOOKPROC) -> windows::core::Result<HHOOK> {
    let module = unsafe { GetModuleHandleW(None)? };
    unsafe { SetWindowsHookExW(id, procedure, Some(HINSTANCE(module.0)), 0) }
}

fn unhook(hook: HHOOK) {
    unsafe {
        let _ = UnhookWindowsHookEx(hook);
    }
}

struct Hooks {
    keyboard: HHOOK,
    mouse: Option<HHOOK>,
}
impl Hooks {
    fn install() -> windows::core::Result<Self> {
        Ok(Self { keyboard: install_hook(WH_KEYBOARD_LL, Some(keyboard_proc))?, mouse: None })
    }

    fn reinstall(&mut self) {
        match install_hook(WH_KEYBOARD_LL, Some(keyboard_proc)) {
            Ok(replacement) => unhook(std::mem::replace(&mut self.keyboard, replacement)),
            Err(error) => log::error!("Keyboard hook reinstall: {error}"),
        }
        if let Some(mouse) = self.mouse {
            match install_hook(WH_MOUSE_LL, Some(mouse_proc)) {
                Ok(replacement) => {
                    unhook(mouse);
                    self.mouse = Some(replacement);
                }
                Err(error) => log::error!("Mouse hook reinstall: {error}"),
            }
        }
    }

    fn sync_mouse(&mut self, capturing: bool) {
        match (capturing, self.mouse) {
            (true, None) => match install_hook(WH_MOUSE_LL, Some(mouse_proc)) {
                Ok(hook) => self.mouse = Some(hook),
                Err(error) => log::error!("Mouse hook: {error}"),
            },
            (false, Some(hook)) => {
                unhook(hook);
                self.mouse = None;
            }
            _ => {}
        }
    }
}
impl Drop for Hooks {
    fn drop(&mut self) {
        unhook(self.keyboard);
        if let Some(mouse) = self.mouse.take() {
            unhook(mouse);
        }
    }
}

fn with_state(action: impl FnOnce(&mut HookState, &Shared)) {
    CONTEXT.with(|context| {
        if let Some(context) = context.borrow_mut().as_mut() {
            action(&mut context.state, &context.shared);
        }
    });
}

fn run_hook_thread(
    shared: Arc<Shared>,
    events: mpsc::Sender<HookEvent>,
    ready: mpsc::SyncSender<windows::core::Result<u32>>,
) {
    raise_current_thread_priority(THREAD_PRIORITY_TIME_CRITICAL);
    let thread_id = unsafe { GetCurrentThreadId() };
    let mut message = MSG::default();
    unsafe {
        let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
    }
    let config = HookConfig::from_bits(shared.config.load(Ordering::Relaxed));
    CONTEXT.with(|context| {
        *context.borrow_mut() = Some(HookContext { state: HookState::new(config), shared, events, thread_id })
    });
    let mut hooks = match Hooks::install() {
        Ok(hooks) => hooks,
        Err(error) => {
            let _ = ready.send(Err(error));
            CONTEXT.with(|c| c.borrow_mut().take());
            return;
        }
    };
    let timer = unsafe { SetTimer(None, 0, REINSTALL_INTERVAL_MS, None) };
    if timer == 0 {
        drop(hooks);
        let _ = ready.send(Err(windows::core::Error::from_thread()));
        CONTEXT.with(|c| c.borrow_mut().take());
        return;
    }
    let _ = ready.send(Ok(thread_id));
    loop {
        if unsafe { GetMessageW(&mut message, None, 0, 0) }.0 <= 0 {
            break;
        }
        match message.message {
            WM_TIMER if message.wParam.0 == timer => {
                hooks.reinstall();
                with_state(|state, _| state.reset(key_is_down));
            }
            REINSTALL => {
                hooks.reinstall();
                with_state(|state, _| state.reset(key_is_down));
            }
            BEGIN_CAPTURE => with_state(|state, shared| {
                state.begin_capture(shared.panel_rect(), now_ms());
                shared.capturing.store(true, Ordering::Relaxed);
            }),
            SET_PANEL_RECT => with_state(|state, shared| state.set_panel_rect(shared.panel_rect())),
            END_CAPTURE => with_state(|state, shared| {
                state.end_capture();
                shared.capturing.store(false, Ordering::Relaxed);
            }),
            _ => {}
        }
        let mut capturing = false;
        with_state(|state, _| capturing = state.is_capturing());
        hooks.sync_mouse(capturing);
    }
    drop(hooks);
    unsafe {
        let _ = KillTimer(None, timer);
    }
    CONTEXT.with(|c| c.borrow_mut().take());
}

/// Win+V / Win+. hotkeys and, between `begin_capture` and `end_capture`, keyboard routing to the panel.
pub struct InputHook {
    shared: Arc<Shared>,
    thread_id: u32,
    hook_thread: Option<JoinHandle<()>>,
    dispatcher: Option<JoinHandle<()>>,
}
impl InputHook {
    /// `sink` runs on a dispatcher thread; post to your UI loop from there.
    pub fn start(config: HookConfig, sink: impl Fn(HookEvent) + Send + Sync + 'static) -> Result<Self> {
        let shared = Arc::new(Shared {
            config: AtomicU8::new(config.bits()),
            capturing: AtomicBool::new(false),
            last_heartbeat_ms: AtomicU64::new(0),
            panel_rect: Mutex::new(RectI::default()),
        });
        let (events, receiver) = mpsc::channel::<HookEvent>();
        let (ready, started) = mpsc::sync_channel(1);
        let dispatcher = thread::Builder::new().name("tuck-hook-events".into()).spawn(move || {
            raise_current_thread_priority(THREAD_PRIORITY_HIGHEST);
            while let Ok(event) = receiver.recv() {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(event)));
                if outcome.is_err() {
                    log::error!("Hook event {event:?} panicked");
                }
            }
        })?;
        let hook_shared = shared.clone();
        let hook_thread = match thread::Builder::new()
            .name("tuck-input-hook".into())
            .spawn(move || run_hook_thread(hook_shared, events, ready))
        {
            Ok(thread) => thread,
            Err(error) => {
                let _ = dispatcher.join();
                return Err(error.into());
            }
        };
        match started.recv().context("Hook thread exited during startup")? {
            Ok(thread_id) => {
                Ok(Self { shared, thread_id, hook_thread: Some(hook_thread), dispatcher: Some(dispatcher) })
            }
            Err(error) => {
                let _ = hook_thread.join();
                let _ = dispatcher.join();
                Err(error.into())
            }
        }
    }

    pub fn set_config(&self, config: HookConfig) {
        self.shared.config.store(config.bits(), Ordering::Relaxed);
    }

    /// Starts routing keys; installs the mouse hook. `panel_rect_px` is in physical screen pixels.
    pub fn begin_capture(&self, panel_rect_px: RectI) {
        self.shared.set_panel_rect(panel_rect_px);
        self.shared.last_heartbeat_ms.store(now_ms(), Ordering::Relaxed);
        self.shared.capturing.store(true, Ordering::Relaxed);
        post(self.thread_id, BEGIN_CAPTURE);
    }

    pub fn set_panel_rect(&self, panel_rect_px: RectI) {
        self.shared.set_panel_rect(panel_rect_px);
        post(self.thread_id, SET_PANEL_RECT);
    }

    pub fn end_capture(&self) {
        self.shared.capturing.store(false, Ordering::Relaxed);
        post(self.thread_id, END_CAPTURE);
    }

    /// False again once the hook ended a stalled capture on its own.
    pub fn is_capturing(&self) -> bool {
        self.shared.capturing.load(Ordering::Relaxed)
    }

    /// Call at least every 250 ms while capturing; after 1000 ms of silence the hook releases the keyboard.
    pub fn heartbeat(&self) {
        self.shared.last_heartbeat_ms.store(now_ms(), Ordering::Relaxed);
    }

    /// Call from the app's WTS_SESSION_UNLOCK handler.
    pub fn on_session_unlock(&self) {
        post(self.thread_id, REINSTALL);
    }
}
impl Drop for InputHook {
    fn drop(&mut self) {
        post(self.thread_id, WM_QUIT);
        if let Some(thread) = self.hook_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.dispatcher.take()
            && thread.thread().id() != thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANEL: RectI = RectI::new(100, 100, 380, 460);
    const KEY_D: u16 = 0x44;
    const KEY_C: u16 = 0x43;
    const KEY_X: u16 = 0x58;
    const LEFT: u16 = 0x25;
    const ESCAPE: u16 = 0x1b;
    const VOLUME_UP: u16 = 0xaf;
    const F5: u16 = 0x74;

    fn enabled() -> HookState {
        HookState::new(HookConfig::from(&ShortcutSettings::default()))
    }

    fn capturing() -> HookState {
        let mut state = enabled();
        state.begin_capture(PANEL, 0);
        state
    }

    fn input(vk: u16, up: bool) -> KeyInput {
        KeyInput { vk, scan: 0x1e, up, ..KeyInput::default() }
    }

    fn press(state: &mut HookState, vk: u16) -> Decision {
        state.on_key(input(vk, false))
    }

    fn release(state: &mut HookState, vk: u16) -> Decision {
        state.on_key(input(vk, true))
    }

    fn routed(decision: Decision) -> RoutedKey {
        assert!(decision.swallow, "{decision:?} should be swallowed");
        match decision.event {
            Some(HookEvent::Key(key)) => key,
            other => panic!("expected a routed key, got {other:?}"),
        }
    }

    fn passes_and_dismisses(decision: Decision) -> bool {
        !decision.swallow && decision.event == Some(HookEvent::Dismiss(DismissReason::Shortcut))
    }

    #[test]
    fn win_v_swallows_down_repeat_and_up_and_masks_start() {
        let mut state = enabled();
        assert_eq!(press(&mut state, vk::LWIN), Decision::default());
        assert_eq!(
            press(&mut state, vk::KEY_V),
            Decision { swallow: true, event: Some(HookEvent::Hotkey(Hotkey::Clipboard)), inject_mask: true }
        );
        assert_eq!(press(&mut state, vk::KEY_V), Decision::swallow_with(None));
        assert_eq!(release(&mut state, vk::KEY_V), Decision::swallow_with(None));
        assert_eq!(release(&mut state, vk::LWIN), Decision::default());
    }

    #[test]
    fn win_period_and_semicolon_open_emoji() {
        for key in [vk::OEM_PERIOD, vk::OEM_1] {
            let mut state = enabled();
            press(&mut state, vk::RWIN);
            assert_eq!(press(&mut state, key).event, Some(HookEvent::Hotkey(Hotkey::Emoji)));
            assert!(release(&mut state, key).swallow);
        }
    }

    #[test]
    fn hotkeys_need_win_alone_and_enabled_config() {
        for modifier in [vk::LSHIFT, vk::LCONTROL, vk::RCONTROL, vk::LMENU, vk::RMENU] {
            let mut state = enabled();
            press(&mut state, vk::LWIN);
            press(&mut state, modifier);
            assert_eq!(press(&mut state, vk::KEY_V), Decision::default());
            assert_eq!(release(&mut state, vk::KEY_V), Decision::default());
        }
        let mut state = HookState::new(HookConfig::default());
        press(&mut state, vk::LWIN);
        for key in [vk::KEY_V, vk::OEM_PERIOD, vk::OEM_1] {
            assert_eq!(press(&mut state, key), Decision::default());
            assert_eq!(release(&mut state, key), Decision::default());
        }
        let mut state = enabled();
        assert_eq!(press(&mut state, vk::KEY_V), Decision::default());
    }

    #[test]
    fn config_bits_round_trip() {
        for config in [
            HookConfig { win_v: true, win_period: false },
            HookConfig { win_v: false, win_period: true },
            HookConfig { win_v: true, win_period: true },
            HookConfig::default(),
        ] {
            assert_eq!(HookConfig::from_bits(config.bits()), config);
        }
    }

    #[test]
    fn outside_capture_everything_else_passes() {
        let mut state = enabled();
        for key in [vk::KEY_A, LEFT, ESCAPE, F5, vk::LCONTROL, KEY_C] {
            assert_eq!(press(&mut state, key), Decision::default());
        }
        for key in [KEY_C, vk::LCONTROL, F5, ESCAPE, LEFT, vk::KEY_A] {
            assert_eq!(release(&mut state, key), Decision::default());
        }
    }

    #[test]
    fn modifiers_pass_and_are_reported_locks_and_media_pass_silently() {
        let mut state = capturing();
        let down = press(&mut state, vk::LCONTROL);
        assert!(!down.swallow);
        assert!(matches!(down.event, Some(HookEvent::Key(key)) if key.mods.ctrl && key.down));
        let up = release(&mut state, vk::LCONTROL);
        assert!(!up.swallow);
        assert!(matches!(up.event, Some(HookEvent::Key(key)) if !key.mods.ctrl && !key.down));
        for key in [vk::CAPITAL, vk::NUMLOCK, vk::SCROLL, VOLUME_UP, vk::BROWSER_BACK, vk::SNAPSHOT] {
            assert_eq!(press(&mut state, key), Decision::default());
            assert_eq!(release(&mut state, key), Decision::default());
        }
        assert!(state.is_capturing());
    }

    #[test]
    fn plain_and_shifted_keys_are_routed_with_paired_ups() {
        let mut state = capturing();
        let key = routed(press(&mut state, vk::KEY_A));
        assert_eq!((key.vk, key.down, key.repeat, key.mods), (vk::KEY_A, true, false, Mods::default()));
        let repeat = routed(press(&mut state, vk::KEY_A));
        assert!(repeat.repeat);
        let up = routed(release(&mut state, vk::KEY_A));
        assert!(!up.down);
        press(&mut state, vk::RSHIFT);
        let shifted = routed(state.on_key(KeyInput { caps_lock: true, ..input(vk::KEY_A, false) }));
        assert!(shifted.mods.shift && shifted.caps_lock);
        assert!(!shifted.mods.ctrl && !shifted.mods.alt && !shifted.mods.win);
        assert!(!routed(press(&mut state, ESCAPE)).mods.ctrl);
    }

    #[test]
    fn altgr_is_text_not_ctrl_alt() {
        let mut state = capturing();
        press(&mut state, vk::LCONTROL);
        press(&mut state, vk::RMENU);
        let decision = press(&mut state, KEY_D);
        assert!(decision.inject_mask);
        let key = routed(decision);
        assert_eq!(key.mods, Mods { altgr: true, ..Mods::default() });
        release(&mut state, KEY_D);
        release(&mut state, vk::LCONTROL);
        assert_eq!(routed(press(&mut state, vk::TAB)).mods, Mods { altgr: true, ..Mods::default() });
        let mut state = capturing();
        press(&mut state, vk::RCONTROL);
        press(&mut state, vk::RMENU);
        assert!(passes_and_dismisses(press(&mut state, KEY_X)));
    }

    #[test]
    fn panel_ctrl_combos_are_routed() {
        for (shift, key) in [
            (false, vk::BACK),
            (false, vk::KEY_A),
            (false, vk::KEY_1),
            (false, vk::KEY_9),
            (false, vk::KEY_P),
            (false, vk::TAB),
            (true, vk::TAB),
            (false, vk::RETURN),
        ] {
            let mut state = capturing();
            press(&mut state, vk::LCONTROL);
            if shift {
                press(&mut state, vk::LSHIFT);
            }
            let routed_key = routed(press(&mut state, key));
            assert!(routed_key.mods.ctrl && routed_key.mods.shift == shift);
            assert!(routed(press(&mut state, key)).repeat);
            assert!(!routed(release(&mut state, key)).down);
        }
    }

    #[test]
    fn other_shortcuts_pass_and_dismiss() {
        let combos: [&[u16]; 6] = [
            &[vk::LCONTROL, KEY_C],
            &[vk::LCONTROL, vk::LSHIFT, vk::KEY_A],
            &[vk::LMENU, vk::TAB],
            &[vk::LCONTROL, vk::LMENU, KEY_X],
            &[vk::LCONTROL, vk::LMENU, vk::KEY_1],
            &[vk::LWIN, KEY_D],
        ];
        for keys in combos {
            let mut state = capturing();
            let (trigger, modifiers) = keys.split_last().unwrap();
            for &modifier in modifiers {
                assert!(!press(&mut state, modifier).swallow);
            }
            assert!(passes_and_dismisses(press(&mut state, *trigger)), "{keys:x?}");
            assert_eq!(press(&mut state, *trigger), Decision::default());
            assert_eq!(release(&mut state, *trigger), Decision::default());
        }
    }

    #[test]
    fn function_keys_pass_and_dismiss() {
        let mut state = capturing();
        for key in [vk::F1, F5, vk::F24] {
            assert!(passes_and_dismisses(press(&mut state, key)));
            assert_eq!(release(&mut state, key), Decision::default());
        }
        press(&mut state, vk::LSHIFT);
        assert!(passes_and_dismisses(press(&mut state, F5)));
    }

    #[test]
    fn opening_win_counts_as_up_until_released() {
        let mut state = enabled();
        press(&mut state, vk::LWIN);
        assert_eq!(press(&mut state, vk::KEY_V).event, Some(HookEvent::Hotkey(Hotkey::Clipboard)));
        state.begin_capture(PANEL, 0);
        assert!(release(&mut state, vk::KEY_V).swallow);
        let decision = press(&mut state, LEFT);
        assert!(decision.inject_mask);
        assert!(!routed(decision).mods.win);
        release(&mut state, LEFT);
        assert!(!routed(press(&mut state, vk::KEY_V)).mods.win);
        release(&mut state, vk::KEY_V);
        assert!(!routed(press(&mut state, vk::OEM_PERIOD)).mods.win);
        release(&mut state, vk::OEM_PERIOD);
        release(&mut state, vk::LWIN);
        press(&mut state, vk::LWIN);
        assert!(passes_and_dismisses(press(&mut state, KEY_D)));
        assert_eq!(press(&mut state, vk::OEM_PERIOD).event, Some(HookEvent::Hotkey(Hotkey::Emoji)));
    }

    #[test]
    fn keys_held_before_capture_pass_their_repeats_and_up() {
        let mut state = enabled();
        press(&mut state, vk::KEY_A);
        state.begin_capture(PANEL, 0);
        assert_eq!(press(&mut state, vk::KEY_A), Decision::default());
        assert_eq!(release(&mut state, vk::KEY_A), Decision::default());
        assert!(routed(press(&mut state, vk::KEY_A)).down);
    }

    #[test]
    fn routed_keys_stay_swallowed_after_capture_ends() {
        let mut state = capturing();
        routed(press(&mut state, vk::RETURN));
        state.end_capture();
        assert_eq!(press(&mut state, vk::RETURN), Decision::swallow_with(None));
        assert_eq!(release(&mut state, vk::RETURN), Decision::swallow_with(None));
        assert_eq!(press(&mut state, vk::RETURN), Decision::default());
    }

    #[test]
    fn stalled_heartbeat_releases_the_keyboard() {
        let mut state = capturing();
        state.heartbeat(500);
        let at = |vk, up, now_ms| KeyInput { now_ms, ..input(vk, up) };
        routed(state.on_key(at(vk::KEY_A, false, 1400)));
        assert!(state.on_key(at(vk::KEY_A, true, 1450)).swallow);
        assert_eq!(
            state.on_key(at(KEY_X, false, 1501)),
            Decision::pass_with(HookEvent::Dismiss(DismissReason::Stalled))
        );
        assert!(!state.is_capturing());
        assert_eq!(state.on_key(at(KEY_X, true, 1502)), Decision::default());
        assert_eq!(state.on_key(at(vk::KEY_A, false, 1503)), Decision::default());
    }

    #[test]
    fn stall_during_a_routed_key_keeps_its_up_paired() {
        let mut state = capturing();
        routed(press(&mut state, LEFT));
        let decision = state.on_key(KeyInput { now_ms: 2000, ..input(LEFT, false) });
        assert_eq!(decision, Decision::swallow_with(Some(HookEvent::Dismiss(DismissReason::Stalled))));
        assert_eq!(release(&mut state, LEFT), Decision::swallow_with(None));
    }

    #[test]
    fn clicks_outside_dismiss_and_stall_is_detected_on_clicks() {
        let mut state = enabled();
        assert_eq!(state.on_button_down(PointI::new(0, 0), 0), Decision::default());
        state.begin_capture(PANEL, 0);
        assert_eq!(state.on_button_down(PointI::new(150, 150), 100), Decision::default());
        assert_eq!(
            state.on_button_down(PointI::new(480, 150), 200),
            Decision::pass_with(HookEvent::Dismiss(DismissReason::ClickOutside))
        );
        state.set_panel_rect(RectI::new(400, 100, 380, 460));
        assert_eq!(state.on_button_down(PointI::new(480, 150), 300), Decision::default());
        assert_eq!(
            state.on_button_down(PointI::new(0, 0), 1301),
            Decision::pass_with(HookEvent::Dismiss(DismissReason::Stalled))
        );
        assert!(!state.is_capturing());
    }

    #[test]
    fn stale_modifiers_and_missed_ups_are_cleared() {
        let mut state = capturing();
        press(&mut state, vk::LCONTROL);
        state.release_stale_keys(KEY_C, false, |_| false);
        assert_eq!(routed(press(&mut state, KEY_C)).mods, Mods::default());
        let mut state = enabled();
        press(&mut state, F5);
        state.begin_capture(PANEL, 0);
        state.release_stale_keys(F5, false, |_| false);
        assert!(passes_and_dismisses(press(&mut state, F5)));
        let mut state = capturing();
        routed(press(&mut state, vk::KEY_A));
        state.release_stale_keys(vk::KEY_A, false, |_| false);
        assert!(routed(press(&mut state, vk::KEY_A)).repeat);
    }

    #[test]
    fn reset_keeps_physically_held_modifiers() {
        let mut state = enabled();
        press(&mut state, vk::LWIN);
        state.begin_capture(PANEL, 0);
        press(&mut state, vk::KEY_A);
        state.reset(|vk| vk == vk::LWIN);
        assert!(!routed(press(&mut state, KEY_X)).mods.win);
        state.reset(|vk| vk == vk::LCONTROL);
        assert!(routed(press(&mut state, vk::KEY_A)).mods.ctrl);
    }

    #[test]
    fn undeliverable_events_let_the_key_through() {
        let mut state = capturing();
        routed(press(&mut state, vk::KEY_A));
        state.forget(vk::KEY_A);
        assert_eq!(press(&mut state, vk::KEY_A), Decision::default());
        assert_eq!(release(&mut state, vk::KEY_A), Decision::default());
    }
}
