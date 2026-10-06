//! Small safe wrappers around the Win32 calls the app needs beyond tuck-ui and tuck-sys.

use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use tuck_core::Image;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, HBITMAP,
    HGDIOBJ,
};
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::System::Threading::{
    ABOVE_NORMAL_PRIORITY_CLASS, GetCurrentProcess, GetCurrentThread, SetPriorityClass, SetThreadPriority,
    THREAD_PRIORITY_HIGHEST,
};
use windows::Win32::UI::HiDpi::GetSystemMetricsForDpi;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowThreadProcessId, HICON, ICONINFO, IsWindow, IsWindowVisible, MSG, PM_NOREMOVE,
    PeekMessageW, SM_CXSMICON, WS_EX_TOOLWINDOW,
};
use windows::core::w;
use windows_core::BOOL;

pub fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

/// False once the window is destroyed (the paste target closed).
pub fn is_window(raw: isize) -> bool {
    // SAFETY: plain query; stale handles return false.
    unsafe { IsWindow(Some(hwnd(raw))) }.as_bool()
}

/// The panel must open instantly while other apps saturate the CPU: the process runs above normal (idle cost is
/// zero) and the UI thread at the highest normal priority.
pub fn raise_ui_priority() {
    // SAFETY: plain calls on pseudo handles of this process and thread.
    let (process, thread) = unsafe {
        (
            SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS),
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST),
        )
    };
    log::info!("priority: process above normal {:?}, UI thread highest {:?}", process.is_ok(), thread.is_ok());
}

/// Notification-area icon size in pixels at `dpi`.
pub fn small_icon_size(dpi: u32) -> u32 {
    // SAFETY: plain metric query.
    let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) };
    if size > 0 { size as u32 } else { (16 * dpi).div_ceil(96) }
}

/// True when the taskbar uses the light theme (`SystemUsesLightTheme`), which decides the tray glyph color.
pub fn taskbar_is_light() -> bool {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: reads one DWORD into a buffer of the stated size.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    status.is_ok() && value != 0
}

/// An HICON destroyed on drop.
pub struct OwnedIcon(HICON);

impl OwnedIcon {
    /// Builds a 32-bit alpha icon from a straight-alpha BGRA image (icons use straight alpha).
    pub fn from_image(image: &Image) -> Result<Self> {
        ensure!(image.width > 0 && image.height > 0, "empty icon image");
        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: image.width as i32,
            biHeight: -(image.height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let info = BITMAPINFO { bmiHeader: header, ..Default::default() };
        let mut bits = std::ptr::null_mut();
        // SAFETY: CreateDIBSection allocates a top-down 32-bpp section; we copy exactly width*height*4 bytes into
        // it, then hand both bitmaps to CreateIconIndirect (which copies them) and delete ours.
        unsafe {
            let color =
                CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).context("CreateDIBSection")?;
            let color = GdiBitmap(color);
            ensure!(!bits.is_null(), "CreateDIBSection returned no pixels");
            std::ptr::copy_nonoverlapping(image.data.as_ptr(), bits.cast::<u8>(), image.data.len());
            let mask_bits = vec![0u8; (image.width.div_ceil(16) * 2 * image.height) as usize];
            let mask =
                GdiBitmap(CreateBitmap(image.width as i32, image.height as i32, 1, 1, Some(mask_bits.as_ptr().cast())));
            let icon_info =
                ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask.0, hbmColor: color.0 };
            let icon = CreateIconIndirect(&icon_info).context("CreateIconIndirect")?;
            Ok(Self(icon))
        }
    }

    pub fn handle(&self) -> HICON {
        self.0
    }
}

impl Drop for OwnedIcon {
    fn drop(&mut self) {
        // SAFETY: we own the icon handle.
        let _ = unsafe { DestroyIcon(self.0) };
    }
}

struct GdiBitmap(HBITMAP);

impl Drop for GdiBitmap {
    fn drop(&mut self) {
        // SAFETY: we own the bitmap handle.
        let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
    }
}

/// Waits up to `limit` for `workers` to finish while answering messages other threads send to this thread's
/// windows (the clipboard thread may wait on our windows). Unfinished workers are left running.
pub fn join_bounded(workers: Vec<JoinHandle<()>>, limit: Duration) {
    let deadline = Instant::now() + limit;
    let mut pending = workers;
    while !pending.is_empty() && Instant::now() < deadline {
        let mut message = MSG::default();
        // SAFETY: PM_NOREMOVE only dispatches incoming sent messages; nothing is removed from the queue.
        let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
        let (finished, running): (Vec<_>, Vec<_>) = pending.into_iter().partition(|w| w.is_finished());
        for worker in finished {
            let _ = worker.join();
        }
        pending = running;
        if !pending.is_empty() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    if !pending.is_empty() {
        log::warn!("{} worker(s) still running at exit", pending.len());
    }
}

/// Glint's per-user install, when present ("Edit in Glint").
pub fn glint_exe() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join(r"Programs\Glint\glint.exe")).filter(|path| path.is_file())
}

/// An app with a visible window, for the "Add app…" menu of ignored apps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningApp {
    /// Lowercase file name, as `HistorySettings::ignored_apps` stores it.
    pub exe_name: String,
    pub name: String,
}

/// Shell hosts own windows but are not apps anyone copies from.
const SHELL_HOSTS: [&str; 6] = [
    "applicationframehost.exe",
    "shellexperiencehost.exe",
    "startmenuexperiencehost.exe",
    "searchhost.exe",
    "textinputhost.exe",
    "sihost.exe",
];

/// Running apps with a visible, unowned, uncloaked top-level window, one per executable, sorted by name.
pub fn running_apps() -> Vec<RunningApp> {
    unsafe extern "system" fn collect(window: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the Vec passed below, valid for the enumeration.
        let windows = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
        windows.push(window);
        true.into()
    }
    let mut windows: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into `windows`, which outlives the enumeration.
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut windows as *mut Vec<HWND> as isize)) };
    let own_pid = std::process::id();
    let mut apps: Vec<RunningApp> = Vec::new();
    for window in windows.into_iter().filter(|w| is_app_window(*w)) {
        let mut pid = 0u32;
        // SAFETY: plain query on a window handle.
        unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
        if pid == own_pid {
            continue;
        }
        let Some(exe) = tuck_sys::target::process_exe(pid) else { continue };
        let Some(exe_name) = exe.file_name().map(|n| n.to_string_lossy().to_lowercase()) else { continue };
        if SHELL_HOSTS.contains(&exe_name.as_str()) || apps.iter().any(|a| a.exe_name == exe_name) {
            continue;
        }
        apps.push(RunningApp { name: tuck_sys::apps::display_name(&exe), exe_name });
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

fn is_app_window(window: HWND) -> bool {
    // SAFETY: plain queries on a window handle from EnumWindows; stale handles fail harmlessly.
    unsafe {
        if !IsWindowVisible(window).as_bool() || GetWindowTextLengthW(window) == 0 {
            return false;
        }
        if GetWindow(window, GW_OWNER).is_ok_and(|owner| !owner.is_invalid()) {
            return false;
        }
        if GetWindowLongPtrW(window, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
        let mut cloaked = 0u32;
        let cloak_read =
            DwmGetWindowAttribute(window, DWMWA_CLOAKED, (&mut cloaked as *mut u32).cast(), size_of::<u32>() as u32);
        cloak_read.is_err() || cloaked == 0
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn AttachConsole(process_id: u32) -> BOOL;
}

/// Lets a GUI-subsystem process print to the console it was started from (`--selftest`, `--help`).
pub fn attach_parent_console() {
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    // SAFETY: no pointers; fails harmlessly when there is no parent console or stdout is already redirected.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// OLE (single-threaded apartment) for shell calls (shortcuts, Explorer, icons) on this thread.
pub struct OleGuard {
    initialized: bool,
}

impl OleGuard {
    pub fn new() -> Self {
        // SAFETY: initializes OLE for the calling thread; balanced in Drop when it succeeded.
        let initialized = unsafe { OleInitialize(None) }.is_ok();
        if !initialized {
            log::warn!("OleInitialize failed; shell calls may not work");
        }
        Self { initialized }
    }
}

impl Default for OleGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for OleGuard {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: balances the successful OleInitialize on this thread.
            unsafe { OleUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_apps_are_unique_and_sorted() {
        let apps = running_apps();
        let unique: std::collections::HashSet<&str> = apps.iter().map(|a| a.exe_name.as_str()).collect();
        assert_eq!(unique.len(), apps.len());
        assert!(apps.windows(2).all(|pair| pair[0].name.to_lowercase() <= pair[1].name.to_lowercase()));
        assert!(apps.iter().all(|a| !SHELL_HOSTS.contains(&a.exe_name.as_str())));
    }
}
