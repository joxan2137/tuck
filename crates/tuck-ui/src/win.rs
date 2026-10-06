//! Small safe wrappers for window chrome and placement. All take raw HWNDs as `isize`.

use std::ffi::c_void;

use tuck_core::{PointI, RectI};
use windows::Win32::Foundation::{COLORREF, FILETIME, HWND, LPARAM, POINT, RECT, SYSTEMTIME, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWM_SYSTEMBACKDROP_TYPE, DWM_WINDOW_CORNER_PREFERENCE, DWMSBT_MAINWINDOW, DWMSBT_TRANSIENTWINDOW,
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_BUTTON_BOUNDS, DWMWA_COLOR_NONE, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_TRANSITIONS_FORCEDISABLED, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DEFAULT,
    DWMWCP_DONOTROUND, DWMWCP_ROUND, DWMWCP_ROUNDSMALL, DWMWINDOWATTRIBUTE, DwmExtendFrameIntoClientArea,
    DwmGetWindowAttribute, DwmSetWindowAttribute,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
use windows::Win32::System::Time::SystemTimeToFileTime;
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    MonitorFromRect,
};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, MOUSEEVENTF_MOVE,
    MOUSEINPUT, SendInput, SetFocus, VK_MENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, DefWindowProcW, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow, GetWindowRect, HWND_NOTOPMOST, HWND_TOPMOST, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SetWindowDisplayAffinity, SetWindowPos, ShowWindow, WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WM_NCACTIVATE,
};
use windows::core::{BOOL, s, w};

use crate::color::Color;

pub(crate) fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

/// Makes the process Per-Monitor-V2 DPI aware. Call first thing in `main` of examples and tests (the app embeds
/// a manifest instead). Harmless if awareness was already set.
pub fn enable_per_monitor_dpi_awareness() {
    // SAFETY: process-wide setting with no pointer arguments.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

fn set_attribute<T>(window: isize, attribute: DWMWINDOWATTRIBUTE, value: &T) -> bool {
    // SAFETY: `value` points to a live T of the size passed.
    unsafe {
        DwmSetWindowAttribute(hwnd(window), attribute, (value as *const T).cast(), size_of::<T>() as u32).is_ok()
    }
}

/// Mica main-window backdrop (DWMSBT_MAINWINDOW). Needs the frame extended into the client area to show.
pub fn set_mica(window: isize, enabled: bool) -> bool {
    let kind = if enabled { DWMSBT_MAINWINDOW } else { DWM_SYSTEMBACKDROP_TYPE(1) };
    set_attribute(window, DWMWA_SYSTEMBACKDROP_TYPE, &kind)
}

/// Transient-window acrylic (DWMSBT_TRANSIENTWINDOW, the material of Windows flyouts) behind the client area;
/// extends the frame over the whole window so it shows through transparent pixels. Set once before the first show.
pub fn set_transient_acrylic(window: isize) -> bool {
    extend_frame_into_client(window) && set_attribute(window, DWMWA_SYSTEMBACKDROP_TYPE, &DWMSBT_TRANSIENTWINDOW)
}

/// The 1 px DWM window border: `None` removes it (DWMWA_COLOR_NONE).
pub fn set_border_color(window: isize, color: Option<Color>) -> bool {
    let value = color.map_or(DWMWA_COLOR_NONE, |c| {
        let [r, g, b, _] = c.to_rgba8();
        COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16).0
    });
    set_attribute(window, DWMWA_BORDER_COLOR, &value)
}

#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    gradient_abgr: u32,
    animation_id: u32,
}

#[repr(C)]
struct CompositionAttributeData {
    attribute: u32,
    data: *mut c_void,
    size: usize,
}

type SetWindowCompositionAttribute = unsafe extern "system" fn(HWND, *mut CompositionAttributeData) -> BOOL;

const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

/// Legacy acrylic blur through the undocumented `SetWindowCompositionAttribute(WCA_ACCENT_POLICY)` with
/// `ACCENT_ENABLE_ACRYLICBLURBEHIND`, tinted with `tint`. Never combine with a DWM system backdrop.
pub fn set_accent_acrylic(window: isize, tint: Color) -> bool {
    // SAFETY: looks up an optional user32 export whose signature matches `SetWindowCompositionAttribute`.
    let function = unsafe {
        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else { return false };
        let Some(proc) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else { return false };
        std::mem::transmute::<unsafe extern "system" fn() -> isize, SetWindowCompositionAttribute>(proc)
    };
    let [r, g, b, a] = tint.to_rgba8();
    let mut policy = AccentPolicy {
        state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
        flags: 0,
        gradient_abgr: (a as u32) << 24 | (b as u32) << 16 | (g as u32) << 8 | r as u32,
        animation_id: 0,
    };
    let mut data = CompositionAttributeData {
        attribute: WCA_ACCENT_POLICY,
        data: (&mut policy as *mut AccentPolicy).cast(),
        size: size_of::<AccentPolicy>(),
    };
    // SAFETY: `data` points to a live accent policy of the size given.
    unsafe { function(hwnd(window), &mut data) }.as_bool()
}

/// Tells DWM the (never-activated) window's frame is active, so materials that only render for active windows
/// keep rendering: `WM_NCACTIVATE(TRUE)` straight through `DefWindowProc`.
pub fn activate_frame(window: isize) {
    // SAFETY: default processing of a message for our own window.
    unsafe { DefWindowProcW(hwnd(window), WM_NCACTIVATE, WPARAM(1), LPARAM(0)) };
}

/// Local time minus UTC right now, in minutes.
pub fn utc_offset_minutes() -> i32 {
    let minutes = |t: SYSTEMTIME| {
        let mut file_time = FILETIME::default();
        // SAFETY: both pointers are valid locals.
        let _ = unsafe { SystemTimeToFileTime(&t, &mut file_time) };
        (((file_time.dwHighDateTime as u64) << 32 | file_time.dwLowDateTime as u64) / 600_000_000) as i64
    };
    // SAFETY: both calls only return the current time.
    let (local, utc) = unsafe { (GetLocalTime(), GetSystemTime()) };
    let delta = (minutes(local) - minutes(utc)) as f32;
    (delta / 15.0).round() as i32 * 15
}

/// Dark title bar and dark Mica tint.
pub fn set_dark_mode(window: isize, dark: bool) -> bool {
    let value = windows::core::BOOL::from(dark);
    set_attribute(window, DWMWA_USE_IMMERSIVE_DARK_MODE, &value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CornerPreference {
    Default,
    Square,
    Round,
    RoundSmall,
}

pub fn set_corner_preference(window: isize, corners: CornerPreference) -> bool {
    let value: DWM_WINDOW_CORNER_PREFERENCE = match corners {
        CornerPreference::Default => DWMWCP_DEFAULT,
        CornerPreference::Square => DWMWCP_DONOTROUND,
        CornerPreference::Round => DWMWCP_ROUND,
        CornerPreference::RoundSmall => DWMWCP_ROUNDSMALL,
    };
    set_attribute(window, DWMWA_WINDOW_CORNER_PREFERENCE, &value)
}

/// Turns off the DWM open/close animation (overlays must appear instantly).
pub fn disable_transitions(window: isize) -> bool {
    set_attribute(window, DWMWA_TRANSITIONS_FORCEDISABLED, &windows::core::BOOL::from(true))
}

/// Extends the DWM frame over the whole client area (needed for Mica and custom title bars).
pub fn extend_frame_into_client(window: isize) -> bool {
    let margins = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
    // SAFETY: margins pointer valid for the call.
    unsafe { DwmExtendFrameIntoClientArea(hwnd(window), &margins).is_ok() }
}

/// Hides the window from screenshots and recordings (WDA_EXCLUDEFROMCAPTURE).
pub fn set_exclude_from_capture(window: isize, exclude: bool) -> bool {
    // SAFETY: plain call on a window handle.
    unsafe { SetWindowDisplayAffinity(hwnd(window), if exclude { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE }).is_ok() }
}

pub fn set_topmost(window: isize, topmost: bool) -> bool {
    let after = if topmost { HWND_TOPMOST } else { HWND_NOTOPMOST };
    // SAFETY: plain call on a window handle.
    unsafe { SetWindowPos(hwnd(window), Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE).is_ok() }
}

/// Marks input Tuck injects itself (the keyboard hook ignores events carrying it): ASCII "GLNT".
pub const INJECTED_INPUT_MARKER: usize = 0x474C_4E54;

fn is_foreground(window: isize) -> bool {
    // SAFETY: plain query.
    let foreground = unsafe { GetForegroundWindow() };
    foreground == hwnd(window)
}

fn send_inputs(inputs: &[INPUT]) {
    // SAFETY: the slice holds fully initialized INPUT records.
    unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
}

fn mouse_nudge() -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: 0, dwFlags: MOUSEEVENTF_MOVE, time: 0, dwExtraInfo: INJECTED_INPUT_MARKER },
        },
    }
}

fn alt_key(up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_MENU,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: INJECTED_INPUT_MARKER,
            },
        },
    }
}

/// Brings a window to the foreground with keyboard focus even when Windows' foreground lock would refuse it (our
/// process got no direct input: hotkeys from a low-level hook, timers). Tries, in order: plain
/// SetForegroundWindow; a zero-length injected mouse move (counts as our input); attaching to the foreground
/// thread's input queue; an injected Alt press/release tagged with `INJECTED_INPUT_MARKER`. Returns true when the
/// window ended up in the foreground. Elevated foreground windows (UIPI) can still refuse.
pub fn force_foreground(window: isize) -> bool {
    let target = hwnd(window);
    // SAFETY: every call operates on window handles and thread ids that are valid for the duration of the call.
    unsafe {
        let _ = SetForegroundWindow(target);
        if !is_foreground(window) {
            send_inputs(&[mouse_nudge()]);
            let _ = SetForegroundWindow(target);
        }
        if !is_foreground(window) {
            let foreground = GetForegroundWindow();
            let foreground_thread = GetWindowThreadProcessId(foreground, None);
            let own_thread = GetCurrentThreadId();
            let attached = foreground_thread != 0
                && foreground_thread != own_thread
                && AttachThreadInput(own_thread, foreground_thread, true).as_bool();
            let _ = BringWindowToTop(target);
            let _ = SetForegroundWindow(target);
            if attached {
                let _ = AttachThreadInput(own_thread, foreground_thread, false);
            }
        }
        if !is_foreground(window) {
            send_inputs(&[alt_key(false)]);
            let _ = SetForegroundWindow(target);
            send_inputs(&[alt_key(true)]);
        }
        let foreground = is_foreground(window);
        if foreground {
            let _ = SetFocus(Some(target));
        }
        foreground
    }
}

pub fn show_no_activate(window: isize) {
    // SAFETY: plain call on a window handle.
    let _ = unsafe { ShowWindow(hwnd(window), SW_SHOWNOACTIVATE) };
}

/// Moves and resizes the window (outer rect) in physical pixels.
pub fn set_window_rect_px(window: isize, rect: RectI) -> bool {
    // SAFETY: plain call on a window handle.
    unsafe { SetWindowPos(hwnd(window), None, rect.x, rect.y, rect.w, rect.h, SWP_NOZORDER | SWP_NOACTIVATE).is_ok() }
}

pub fn window_rect_px(window: isize) -> Option<RectI> {
    let mut r = RECT::default();
    // SAFETY: out-pointer valid.
    unsafe { GetWindowRect(hwnd(window), &mut r) }.ok()?;
    Some(RectI::from_ltrb(r.left, r.top, r.right, r.bottom))
}

pub fn client_origin_px(window: isize) -> Option<PointI> {
    let mut p = POINT::default();
    // SAFETY: out-pointer valid.
    unsafe { ClientToScreen(hwnd(window), &mut p) }.as_bool().then_some(PointI::new(p.x, p.y))
}

/// Caption button bounds relative to the client area, in physical pixels.
pub fn caption_buttons_rect_px(window: isize) -> Option<RectI> {
    let mut r = RECT::default();
    // SAFETY: out-pointer valid and sized as the attribute expects.
    unsafe {
        DwmGetWindowAttribute(hwnd(window), DWMWA_CAPTION_BUTTON_BOUNDS, (&mut r as *mut RECT).cast(), size_of::<RECT>() as u32)
    }
    .ok()?;
    let window_rect = window_rect_px(window)?;
    let origin = client_origin_px(window)?;
    let (dx, dy) = (window_rect.x - origin.x, window_rect.y - origin.y);
    Some(RectI::from_ltrb(r.left + dx, r.top + dy, r.right + dx, r.bottom + dy))
}

pub fn dpi_for_window(window: isize) -> u32 {
    // SAFETY: plain query.
    unsafe { GetDpiForWindow(hwnd(window)) }.max(96)
}

fn monitor_dpi(monitor: HMONITOR) -> u32 {
    let (mut x, mut y) = (96u32, 96u32);
    // SAFETY: out-pointers valid.
    let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) };
    x.max(96)
}

/// Effective DPI of the monitor nearest to a physical-pixel point.
pub fn dpi_at_point(point: PointI) -> u32 {
    // SAFETY: plain query.
    monitor_dpi(unsafe { MonitorFromPoint(POINT { x: point.x, y: point.y }, MONITOR_DEFAULTTONEAREST) })
}

/// Effective DPI of the monitor that contains most of a physical-pixel rect.
pub fn dpi_for_rect(rect: RectI) -> u32 {
    let r = RECT { left: rect.x, top: rect.y, right: rect.right(), bottom: rect.bottom() };
    // SAFETY: plain query.
    monitor_dpi(unsafe { MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST) })
}

/// Work area (monitor minus taskbar) of the monitor nearest to a physical-pixel point.
pub fn work_area_at_point(point: PointI) -> Option<RectI> {
    // SAFETY: plain queries with valid out-pointers.
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: point.x, y: point.y }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        GetMonitorInfoW(monitor, &mut info).as_bool().then(|| {
            let w = info.rcWork;
            RectI::from_ltrb(w.left, w.top, w.right, w.bottom)
        })
    }
}
