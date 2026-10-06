use crate::util::OwnedHandle;
use anyhow::{Result, ensure};
use std::{cell::RefCell, ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf, rc::Rc};
use tuck_core::{PointI, RectI};
use windows::{
    Win32::{
        Foundation::{HANDLE, HWND, POINT, RECT},
        Graphics::Gdi::{
            GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MapWindowPoints, MonitorFromPoint,
            MonitorFromRect, MonitorFromWindow,
        },
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{
            OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
        UI::{
            Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
            HiDpi::{
                GetDpiForMonitor, GetWindowDpiAwarenessContext, LogicalToPhysicalPointForPerMonitorDPI,
                MDT_EFFECTIVE_DPI, SetThreadDpiAwarenessContext,
            },
            Input::KeyboardAndMouse::GetKeyboardLayout,
            WindowsAndMessaging::{
                EVENT_SYSTEM_FOREGROUND, GUITHREADINFO, GetCursorPos, GetForegroundWindow, GetGUIThreadInfo,
                GetWindowThreadProcessId, WINEVENT_OUTOFCONTEXT,
            },
        },
    },
    core::PWSTR,
};

/// The window that had the foreground when a hotkey fired; text and pastes go back to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub hwnd: isize,
    pub focus: isize,
    pub thread_id: u32,
    pub pid: u32,
    pub exe: Option<PathBuf>,
    /// HKL of the target thread, for `TextTranslator::translate`.
    pub layout: isize,
    /// Physical screen pixels, from GetGUIThreadInfo.
    pub caret: Option<RectI>,
    /// Work area of the monitor holding the caret, else the window.
    pub work_area: RectI,
    /// Injected input cannot reach it (UIPI). False when the token cannot be read.
    pub elevated: bool,
}
impl Target {
    pub fn is_foreground(&self) -> bool {
        unsafe { GetForegroundWindow() }.0 as isize == self.hwnd
    }

    /// Lowercase executable file name, as `PasteSettings::key_for` and `ignored_apps` expect.
    pub fn exe_name(&self) -> Option<String> {
        Some(self.exe.as_ref()?.file_name()?.to_string_lossy().to_lowercase())
    }
}

fn rect_i(rect: RECT) -> RectI {
    RectI::from_ltrb(rect.left, rect.top, rect.right, rect.bottom)
}

/// Snapshot of the foreground window, its focus, caret and process. Cheap (well under 1 ms); never blocks on the
/// target's message loop.
pub fn capture_target() -> Option<Target> {
    let window = unsafe { GetForegroundWindow() };
    if window.is_invalid() {
        return None;
    }
    let mut pid = 0;
    let thread_id = unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    if thread_id == 0 {
        return None;
    }
    let mut info = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    let has_info = unsafe { GetGUIThreadInfo(thread_id, &mut info) }.is_ok();
    let focus = if has_info && !info.hwndFocus.is_invalid() { info.hwndFocus } else { window };
    let caret = if has_info { caret_rect(&info) } else { None };
    let process = open_process(pid);
    Some(Target {
        hwnd: window.0 as isize,
        focus: focus.0 as isize,
        thread_id,
        pid,
        exe: process.as_ref().and_then(image_path),
        layout: unsafe { GetKeyboardLayout(thread_id) }.0 as isize,
        caret,
        work_area: caret.map_or_else(|| window_work_area(window), work_area_of),
        elevated: process.as_ref().is_some_and(is_elevated),
    })
}

fn caret_rect(info: &GUITHREADINFO) -> Option<RectI> {
    let local = rect_i(info.rcCaret);
    if info.hwndCaret.is_invalid() || local.is_empty() {
        return None;
    }
    let mut corners = [POINT { x: local.x, y: local.y }, POINT { x: local.right(), y: local.bottom() }];
    to_physical_screen(info.hwndCaret, &mut corners);
    let [top_left, bottom_right] = corners;
    Some(RectI::from_ltrb(top_left.x, top_left.y, bottom_right.x, bottom_right.y))
}

/// Maps client points of `window` to physical screen pixels, also for DPI-virtualized (unaware) windows.
fn to_physical_screen(window: HWND, points: &mut [POINT]) {
    let previous = unsafe { SetThreadDpiAwarenessContext(GetWindowDpiAwarenessContext(window)) };
    unsafe { MapWindowPoints(Some(window), None, points) };
    if previous.0.is_null() {
        return;
    }
    unsafe { SetThreadDpiAwarenessContext(previous) };
    for point in points {
        let _ = unsafe { LogicalToPhysicalPointForPerMonitorDPI(Some(window), point) };
    }
}

fn open_process(pid: u32) -> Option<OwnedHandle> {
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok().map(OwnedHandle)
}

fn image_path(process: &OwnedHandle) -> Option<PathBuf> {
    let mut buffer = [0u16; 1024];
    let mut length = buffer.len() as u32;
    unsafe { QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut length) }
        .ok()?;
    Some(OsString::from_wide(&buffer[..length as usize]).into())
}

fn is_elevated(process: &OwnedHandle) -> bool {
    let mut token = HANDLE::default();
    if unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let token = OwnedHandle(token);
    let mut elevation = TOKEN_ELEVATION::default();
    let mut length = 0;
    let read = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut length,
        )
    };
    read.is_ok() && elevation.TokenIsElevated != 0
}

/// Full path of a process's executable, if it can be queried.
pub fn process_exe(pid: u32) -> Option<PathBuf> {
    image_path(&open_process(pid)?)
}

fn monitor_work_area(monitor: HMONITOR) -> RectI {
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = unsafe { GetMonitorInfoW(monitor, &mut info) };
    rect_i(info.rcWork)
}

fn window_work_area(window: HWND) -> RectI {
    monitor_work_area(unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) })
}

fn work_area_of(rect: RectI) -> RectI {
    let rect = RECT { left: rect.x, top: rect.y, right: rect.right(), bottom: rect.bottom() };
    monitor_work_area(unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST) })
}

fn monitor_at(point: PointI) -> HMONITOR {
    unsafe { MonitorFromPoint(POINT { x: point.x, y: point.y }, MONITOR_DEFAULTTONEAREST) }
}

/// Work area (physical pixels) of the monitor nearest to `point`.
pub fn work_area_at(point: PointI) -> RectI {
    monitor_work_area(monitor_at(point))
}

/// DPI scale (1.0 = 96 dpi) of the monitor nearest to `point`.
pub fn scale_at(point: PointI) -> f32 {
    let (mut dpi_x, mut dpi_y) = (96, 96);
    let _ = unsafe { GetDpiForMonitor(monitor_at(point), MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
    dpi_x as f32 / 96.0
}

/// Mouse cursor in physical screen pixels.
pub fn cursor_pos() -> PointI {
    let mut point = POINT::default();
    let _ = unsafe { GetCursorPos(&mut point) };
    PointI::new(point.x, point.y)
}

type ForegroundSink = Rc<dyn Fn(isize)>;
thread_local! { static WATCHES: RefCell<Vec<(isize, ForegroundSink)>> = const { RefCell::new(Vec::new()) }; }

unsafe extern "system" fn foreground_changed(
    hook: HWINEVENTHOOK,
    _event: u32,
    window: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    let sink = WATCHES.with(|watches| {
        watches.borrow().iter().find(|(handle, _)| *handle == hook.0 as isize).map(|(_, sink)| sink.clone())
    });
    if let Some(sink) = sink {
        sink(window.0 as isize);
    }
}

/// Reports every foreground change (`sink(hwnd)`) on the thread that started it, which must pump messages.
pub struct ForegroundWatch {
    hook: HWINEVENTHOOK,
}
impl ForegroundWatch {
    pub fn start(sink: impl Fn(isize) + 'static) -> Result<Self> {
        let hook = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(foreground_changed),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        };
        ensure!(!hook.is_invalid(), "Could not watch foreground changes");
        WATCHES.with(|watches| watches.borrow_mut().push((hook.0 as isize, Rc::new(sink))));
        Ok(Self { hook })
    }
}
impl Drop for ForegroundWatch {
    fn drop(&mut self) {
        unsafe {
            let _ = UnhookWinEvent(self.hook);
        }
        let handle = self.hook.0 as isize;
        WATCHES.with(|watches| watches.borrow_mut().retain(|(watched, _)| *watched != handle));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_name_is_lowercase_file_name() {
        let target = Target {
            hwnd: 0,
            focus: 0,
            thread_id: 0,
            pid: 0,
            exe: Some(PathBuf::from(r"C:\Program Files\PuTTY\PUTTY.EXE")),
            layout: 0,
            caret: None,
            work_area: RectI::default(),
            elevated: false,
        };
        assert_eq!(target.exe_name().as_deref(), Some("putty.exe"));
        assert_eq!(Target { exe: None, ..target }.exe_name(), None);
    }
}
