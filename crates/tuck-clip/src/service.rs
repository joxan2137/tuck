use std::{
    cell::Cell,
    ffi::OsString,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, ensure};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_CLASS_ALREADY_EXISTS, GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM,
        },
        System::{
            DataExchange::{
                AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardOwner,
                GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
                RemoveClipboardFormatListener, SetClipboardData,
            },
            LibraryLoader::GetModuleHandleW,
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
            Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT, DROPEFFECT_COPY},
            Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
            },
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow, GetMessageW,
            GetWindowThreadProcessId, HWND_MESSAGE, KillTimer, MSG, PostMessageW, RegisterClassExW, SetTimer,
            WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLIPBOARDUPDATE, WM_TIMER, WNDCLASSEXW,
        },
    },
    core::{PCWSTR, PWSTR, w},
};

use crate::{CaptureFilter, Captured, ClipEvent, SkipReason, WritePayload, formats};

const UNICODE_TEXT: u32 = CF_UNICODETEXT.0 as u32;
const HDROP: u32 = CF_HDROP.0 as u32;
const DIBV5: u32 = CF_DIBV5.0 as u32;
const DIB: u32 = CF_DIB.0 as u32;

const WINDOW_CLASS: PCWSTR = w!("Tuck.Clipboard");
const WM_RUN_COMMANDS: u32 = WM_APP + 1;
const SETTLE_TIMER: usize = 1;
const SETTLE_DELAY_MS: u32 = 40;
/// A clipboard that keeps changing is still captured this long after its first change.
const MAX_SETTLE: Duration = Duration::from_millis(250);
const OPEN_ATTEMPTS: u32 = 8;
const STOP_TIMEOUT: Duration = Duration::from_secs(1);
const SHELL_HOSTS: [&str; 7] = [
    "explorer.exe",
    "sihost.exe",
    "shellexperiencehost.exe",
    "startmenuexperiencehost.exe",
    "searchhost.exe",
    "textinputhost.exe",
    "applicationframehost.exe",
];

type Sink = Box<dyn Fn(ClipEvent) + Send>;
type WriteDone = Box<dyn FnOnce(Result<()>) + Send>;

enum Command {
    Write { payload: WritePayload, token: u64, done: WriteDone },
    Stop,
}

/// Owns the clipboard thread. Events reach the sink on that thread; dropping the service stops it.
pub struct ClipboardService {
    window: isize,
    filter: Arc<Mutex<CaptureFilter>>,
    commands: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl ClipboardService {
    pub fn start(filter: CaptureFilter, sink: impl Fn(ClipEvent) + Send + 'static) -> Result<Self> {
        let filter = Arc::new(Mutex::new(filter));
        let (commands, command_queue) = mpsc::channel();
        let (ready_sender, ready) = mpsc::sync_channel(1);
        let thread_filter = filter.clone();
        let thread = thread::Builder::new()
            .name("tuck-clipboard".into())
            .spawn(move || run_clipboard_thread(thread_filter, Box::new(sink), command_queue, ready_sender))?;
        match ready.recv() {
            Ok(Ok(window)) => Ok(Self { window, filter, commands, thread: Some(thread) }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(anyhow!("The clipboard thread exited during startup"))
            }
        }
    }

    pub fn set_filter(&self, filter: CaptureFilter) {
        *self.filter.lock().unwrap_or_else(PoisonError::into_inner) = filter;
    }

    /// Replaces the clipboard with `payload` plus the `Tuck.Token` format, then calls `done` on the clipboard thread.
    pub fn write(&self, payload: WritePayload, token: u64, done: impl FnOnce(Result<()>) + Send + 'static) {
        match self.commands.send(Command::Write { payload, token, done: Box::new(done) }) {
            Ok(()) => self.wake(),
            Err(mpsc::SendError(Command::Write { done, .. })) => done(Err(anyhow!("The clipboard thread has stopped"))),
            Err(mpsc::SendError(Command::Stop)) => {}
        }
    }

    fn wake(&self) {
        let window = HWND(self.window as _);
        if let Err(error) = unsafe { PostMessageW(Some(window), WM_RUN_COMMANDS, WPARAM(0), LPARAM(0)) } {
            log::warn!("Could not wake the clipboard thread: {error}");
        }
    }
}

impl Drop for ClipboardService {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        self.wake();
        let Some(thread) = self.thread.take() else { return };
        let deadline = Instant::now() + STOP_TIMEOUT;
        while !thread.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if thread.is_finished() {
            let _ = thread.join();
        } else {
            log::warn!("The clipboard thread is blocked by a clipboard owner; leaving it behind");
        }
    }
}

fn run_clipboard_thread(
    filter: Arc<Mutex<CaptureFilter>>,
    sink: Sink,
    commands: mpsc::Receiver<Command>,
    ready: mpsc::SyncSender<Result<isize>>,
) {
    let window = match ListenerWindow::create() {
        Ok(window) => window,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(window.0.0 as isize));
    let mut watcher = Watcher {
        window: window.0,
        formats: Formats::register(),
        filter,
        sink,
        last_sequence: unsafe { GetClipboardSequenceNumber() },
    };
    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        match message.message {
            WM_TIMER if message.wParam.0 == SETTLE_TIMER => {
                unsafe {
                    let _ = KillTimer(Some(window.0), SETTLE_TIMER);
                }
                SETTLING_SINCE.set(None);
                watcher.on_settled();
            }
            WM_RUN_COMMANDS => {
                if !watcher.run_commands(&commands) {
                    break;
                }
            }
            _ => unsafe {
                DispatchMessageW(&message);
            },
        }
    }
}

thread_local! {
    static SETTLING_SINCE: Cell<Option<Instant>> = const { Cell::new(None) };
}

struct ListenerWindow(HWND);

impl ListenerWindow {
    fn create() -> Result<Self> {
        let instance = unsafe { GetModuleHandleW(None) }?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: WINDOW_CLASS,
            ..Default::default()
        };
        if unsafe { RegisterClassExW(&class) } == 0 {
            let error = windows::core::Error::from_thread();
            ensure!(error.code() == ERROR_CLASS_ALREADY_EXISTS.to_hresult(), error);
        }
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                WINDOW_CLASS,
                w!("Tuck clipboard"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance.into()),
                None,
            )
        }?;
        let window = Self(window);
        unsafe { AddClipboardFormatListener(window.0) }?;
        Ok(window)
    }
}

impl Drop for ListenerWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = RemoveClipboardFormatListener(self.0);
            let _ = DestroyWindow(self.0);
        }
    }
}

extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_CLIPBOARDUPDATE {
        let since = SETTLING_SINCE.get().unwrap_or_else(Instant::now);
        SETTLING_SINCE.set(Some(since));
        if since.elapsed() < MAX_SETTLE {
            unsafe { SetTimer(Some(window), SETTLE_TIMER, SETTLE_DELAY_MS, None) };
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

struct Formats {
    png: u32,
    html: u32,
    rtf: u32,
    token: u32,
    exclude_from_monitor: u32,
    viewer_ignore: u32,
    history_permission: u32,
    drop_effect: u32,
}

impl Formats {
    fn register() -> Self {
        let register = |name: PCWSTR| unsafe { RegisterClipboardFormatW(name) };
        Self {
            png: register(w!("PNG")),
            html: register(w!("HTML Format")),
            rtf: register(w!("Rich Text Format")),
            token: register(w!("Tuck.Token")),
            exclude_from_monitor: register(w!("ExcludeClipboardContentFromMonitorProcessing")),
            viewer_ignore: register(w!("Clipboard Viewer Ignore")),
            history_permission: register(w!("CanIncludeInClipboardHistory")),
            drop_effect: register(w!("Preferred DropEffect")),
        }
    }

    /// Every (format, bytes) pair a write sets, encoded before the clipboard is opened.
    fn entries(&self, payload: &WritePayload, token: u64) -> Result<Vec<(u32, Vec<u8>)>> {
        let mut entries = Vec::new();
        if let Some(text) = &payload.text {
            entries.push((UNICODE_TEXT, formats::encode_utf16_text(text)));
        }
        if let Some(html) = &payload.html {
            entries.push((self.html, formats::encode_utf8_text(html)));
        }
        if let Some(rtf) = &payload.rtf {
            entries.push((self.rtf, formats::encode_ansi_text(rtf)));
        }
        if let Some(image) = &payload.image {
            entries.push((self.png, formats::encode_png(image)?));
            entries.push((DIBV5, formats::image_to_dibv5(image)));
        }
        if !payload.files.is_empty() {
            entries.push((HDROP, formats::build_hdrop(&payload.files)));
            entries.push((self.drop_effect, DROPEFFECT_COPY.0.to_le_bytes().to_vec()));
        }
        ensure!(!entries.is_empty(), "Nothing to write to the clipboard");
        entries.push((self.token, token.to_le_bytes().to_vec()));
        Ok(entries)
    }
}

struct Watcher {
    window: HWND,
    formats: Formats,
    filter: Arc<Mutex<CaptureFilter>>,
    sink: Sink,
    last_sequence: u32,
}

impl Watcher {
    fn on_settled(&mut self) {
        let sequence = unsafe { GetClipboardSequenceNumber() };
        if sequence == self.last_sequence {
            return;
        }
        self.last_sequence = sequence;
        let started = Instant::now();
        let event = self.capture(sequence);
        log_event(&event, started.elapsed());
        (self.sink)(event);
    }

    /// Runs queued commands; false once asked to stop.
    fn run_commands(&self, commands: &mpsc::Receiver<Command>) -> bool {
        for command in commands.try_iter() {
            match command {
                Command::Write { payload, token, done } => done(self.write(&payload, token)),
                Command::Stop => return false,
            }
        }
        true
    }

    fn write(&self, payload: &WritePayload, token: u64) -> Result<()> {
        let started = Instant::now();
        let entries = self.formats.entries(payload, token)?;
        OpenedClipboard::open(self.window)?.replace(&entries)?;
        let bytes: usize = entries.iter().map(|(_, bytes)| bytes.len()).sum();
        log::debug!(
            "Clipboard: wrote {} formats, {bytes} bytes in {} ms",
            entries.len(),
            started.elapsed().as_millis()
        );
        Ok(())
    }

    fn capture(&self, sequence: u32) -> ClipEvent {
        let filter = self.filter.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let clipboard = match OpenedClipboard::open(self.window) {
            Ok(clipboard) => clipboard,
            Err(error) => {
                log::warn!("Clipboard: {error:#}");
                return ClipEvent::Skipped(SkipReason::Unreadable);
            }
        };
        if self.is_excluded(&clipboard) {
            return ClipEvent::Skipped(SkipReason::Excluded);
        }
        if let Some(token) = self.own_token(&clipboard) {
            return ClipEvent::OwnWrite { token };
        }
        if filter.paused {
            return ClipEvent::Skipped(SkipReason::Paused);
        }
        let source_exe = source_exe();
        if source_exe.as_deref().is_some_and(|exe| filter.ignores(exe)) {
            return ClipEvent::Skipped(SkipReason::IgnoredApp);
        }
        match self.read_content(&clipboard, filter.max_bytes) {
            Ok(content) => ClipEvent::Captured(Captured { sequence, source_exe, ..content }),
            Err(reason) => ClipEvent::Skipped(reason),
        }
    }

    fn is_excluded(&self, clipboard: &OpenedClipboard) -> bool {
        let history_allowed = || {
            clipboard
                .read(self.formats.history_permission, |bytes| Ok(bytes.get(..4).is_some_and(|dword| dword != [0; 4])))
                .unwrap_or(false)
        };
        clipboard.has(self.formats.exclude_from_monitor)
            || clipboard.has(self.formats.viewer_ignore)
            || (clipboard.has(self.formats.history_permission) && !history_allowed())
    }

    fn own_token(&self, clipboard: &OpenedClipboard) -> Option<u64> {
        if !clipboard.has(self.formats.token) {
            return None;
        }
        clipboard
            .read(self.formats.token, |bytes| {
                bytes
                    .get(..8)
                    .and_then(|token| token.try_into().ok())
                    .map(u64::from_le_bytes)
                    .ok_or(SkipReason::Unreadable)
            })
            .ok()
    }

    /// The first of files, text, image that is present; `Unreadable` only when nothing else could be read.
    fn read_content(&self, clipboard: &OpenedClipboard, max_bytes: u64) -> Result<Captured, SkipReason> {
        let mut failed = false;
        for read in [Self::read_files, Self::read_text, Self::read_image] {
            match read(self, clipboard, max_bytes) {
                Ok(Some(content)) => return Ok(content),
                Ok(None) => {}
                Err(SkipReason::Unreadable) => failed = true,
                Err(reason) => return Err(reason),
            }
        }
        Err(if failed { SkipReason::Unreadable } else { SkipReason::Empty })
    }

    fn read_files(&self, clipboard: &OpenedClipboard, max_bytes: u64) -> Result<Option<Captured>, SkipReason> {
        if !clipboard.has(HDROP) {
            return Ok(None);
        }
        let files = clipboard.read(HDROP, |bytes| formats::parse_hdrop(bytes).map_err(unreadable))?;
        if files.is_empty() {
            return Ok(None);
        }
        fits(formats::files_bytes(&files), max_bytes)?;
        Ok(Some(Captured { files, ..Captured::default() }))
    }

    /// Text that fits; HTML and RTF are kept only while the total still fits.
    fn read_text(&self, clipboard: &OpenedClipboard, max_bytes: u64) -> Result<Option<Captured>, SkipReason> {
        if !clipboard.has(UNICODE_TEXT) {
            return Ok(None);
        }
        let text = clipboard.read(UNICODE_TEXT, |bytes| {
            let text_bytes = formats::utf16_until_nul(bytes);
            fits(text_bytes.len() as u64 / 2, max_bytes)?;
            Ok(formats::decode_utf16_text(text_bytes))
        })?;
        if text.is_empty() {
            return Ok(None);
        }
        fits(text.len() as u64, max_bytes)?;
        let mut budget = max_bytes - text.len() as u64;
        let html = read_rich_text(clipboard, self.formats.html, formats::decode_utf8_text, &mut budget);
        let rtf = read_rich_text(clipboard, self.formats.rtf, formats::decode_ansi_text, &mut budget);
        Ok(Some(Captured { text: Some(text), html, rtf, ..Captured::default() }))
    }

    fn read_image(&self, clipboard: &OpenedClipboard, max_bytes: u64) -> Result<Option<Captured>, SkipReason> {
        type Dimensions = fn(&[u8]) -> Option<(u32, u32)>;
        type Decode = fn(&[u8]) -> Result<tuck_core::Image>;
        let sources: [(u32, Dimensions, Decode); 3] = [
            (self.formats.png, formats::png_dimensions, formats::decode_png),
            (DIBV5, formats::dib_dimensions, formats::dib_to_image),
            (DIB, formats::dib_dimensions, formats::dib_to_image),
        ];
        let mut failed = false;
        for (format, dimensions, decode) in sources {
            if !clipboard.has(format) {
                continue;
            }
            let image = clipboard.read(format, |bytes| {
                let (width, height) = dimensions(bytes).ok_or(SkipReason::Unreadable)?;
                fits(formats::image_bytes(width, height), max_bytes)?;
                decode(bytes).map_err(unreadable)
            });
            match image {
                Ok(image) => return Ok(Some(Captured { image: Some(image), ..Captured::default() })),
                Err(SkipReason::Unreadable) => failed = true,
                Err(reason) => return Err(reason),
            }
        }
        if failed { Err(SkipReason::Unreadable) } else { Ok(None) }
    }
}

fn read_rich_text(
    clipboard: &OpenedClipboard,
    format: u32,
    decode: fn(&[u8]) -> String,
    budget: &mut u64,
) -> Option<String> {
    if !clipboard.has(format) {
        return None;
    }
    let text = clipboard
        .read(format, |bytes| {
            let text_bytes = formats::bytes_until_nul(bytes);
            fits(text_bytes.len() as u64, *budget)?;
            Ok(decode(text_bytes))
        })
        .ok()
        .filter(|text| !text.is_empty() && text.len() as u64 <= *budget)?;
    *budget -= text.len() as u64;
    Some(text)
}

fn fits(bytes: u64, max_bytes: u64) -> Result<(), SkipReason> {
    if bytes <= max_bytes { Ok(()) } else { Err(SkipReason::TooLarge) }
}

fn unreadable(error: anyhow::Error) -> SkipReason {
    log::debug!("Clipboard: unreadable data: {error:#}");
    SkipReason::Unreadable
}

fn log_event(event: &ClipEvent, elapsed: Duration) {
    match event {
        ClipEvent::Captured(captured) => log::debug!(
            "Clipboard: captured {:?}, {} bytes, from {} in {} ms",
            captured.kind(),
            captured.byte_len(),
            captured
                .source_exe
                .as_deref()
                .and_then(Path::file_name)
                .map_or("unknown app".into(), |name| name.to_string_lossy()),
            elapsed.as_millis()
        ),
        ClipEvent::OwnWrite { .. } => log::debug!("Clipboard: own write"),
        ClipEvent::Skipped(reason) => log::debug!("Clipboard: skipped ({reason:?})"),
    }
}

/// Delays between the `OPEN_ATTEMPTS` tries of `OpenClipboard`: 10 ms doubling up to 80 ms.
fn open_retry_delays() -> impl Iterator<Item = Duration> {
    (0..OPEN_ATTEMPTS - 1).map(|retry| Duration::from_millis((10u64 << retry).min(80)))
}

/// Proof that this thread has the clipboard open; closes it on drop.
struct OpenedClipboard;

impl OpenedClipboard {
    fn open(owner: HWND) -> Result<Self> {
        let mut delays = open_retry_delays();
        loop {
            match unsafe { OpenClipboard(Some(owner)) } {
                Ok(()) => return Ok(Self),
                Err(error) => match delays.next() {
                    Some(delay) => thread::sleep(delay),
                    None => return Err(anyhow::Error::from(error).context("The clipboard stayed busy")),
                },
            }
        }
    }

    fn has(&self, format: u32) -> bool {
        format != 0 && unsafe { IsClipboardFormatAvailable(format) }.is_ok()
    }

    /// Lends the bytes of a global-memory format to `parse` without copying them.
    fn read<T>(&self, format: u32, parse: impl FnOnce(&[u8]) -> Result<T, SkipReason>) -> Result<T, SkipReason> {
        let handle = unsafe { GetClipboardData(format) }.map_err(|_| SkipReason::Unreadable)?;
        let memory = HGLOBAL(handle.0);
        let data = unsafe { GlobalLock(memory) };
        if data.is_null() {
            return Err(SkipReason::Unreadable);
        }
        let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), GlobalSize(memory)) };
        let parsed = parse(bytes);
        unsafe {
            let _ = GlobalUnlock(memory);
        }
        parsed
    }

    fn replace(&self, entries: &[(u32, Vec<u8>)]) -> Result<()> {
        unsafe { EmptyClipboard() }?;
        entries.iter().try_for_each(|(format, bytes)| put_global(*format, bytes))
    }
}

impl Drop for OpenedClipboard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn put_global(format: u32, bytes: &[u8]) -> Result<()> {
    ensure!(format != 0, "Could not register a clipboard format");
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
        let destination = GlobalLock(memory);
        if destination.is_null() {
            let error = windows::core::Error::from_thread();
            let _ = GlobalFree(Some(memory));
            return Err(error.into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), destination.cast(), bytes.len());
        let _ = GlobalUnlock(memory);
        if let Err(error) = SetClipboardData(format, Some(HANDLE(memory.0))) {
            let _ = GlobalFree(Some(memory));
            return Err(error.into());
        }
    }
    Ok(())
}

/// The clipboard owner's executable, or the foreground window's when the owner is missing or a shell host.
fn source_exe() -> Option<PathBuf> {
    let owner_exe = unsafe { GetClipboardOwner() }.ok().and_then(window_exe);
    match owner_exe {
        Some(exe) if !is_shell_host(&exe) => Some(exe),
        owner_exe => window_exe(unsafe { GetForegroundWindow() }).or(owner_exe),
    }
}

fn is_shell_host(exe: &Path) -> bool {
    exe.file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .is_some_and(|name| SHELL_HOSTS.contains(&name.as_str()))
}

fn window_exe(window: HWND) -> Option<PathBuf> {
    if window.is_invalid() {
        return None;
    }
    let mut process_id = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&mut process_id)) };
    if process_id == 0 {
        return None;
    }
    process_exe(process_id)
}

fn process_exe(process_id: u32) -> Option<PathBuf> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }.ok()?;
    let mut buffer = vec![0u16; 1024];
    let mut len = buffer.len() as u32;
    let queried =
        unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut len) };
    unsafe {
        let _ = CloseHandle(process);
    }
    queried.ok()?;
    Some(OsString::from_wide(&buffer[..len as usize]).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_formats() -> Formats {
        Formats {
            png: 0xC001,
            html: 0xC002,
            rtf: 0xC003,
            token: 0xC004,
            exclude_from_monitor: 0xC005,
            viewer_ignore: 0xC006,
            history_permission: 0xC007,
            drop_effect: 0xC008,
        }
    }

    #[test]
    fn service_can_move_between_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ClipboardService>();
    }

    #[test]
    fn open_retries_back_off_from_10_to_80_ms() {
        let delays: Vec<u64> = open_retry_delays().map(|delay| delay.as_millis() as u64).collect();
        assert_eq!(delays, [10, 20, 40, 80, 80, 80, 80]);
    }

    #[test]
    fn write_entries_cover_every_present_format_and_end_with_the_token() {
        let registered = fake_formats();
        let payload = WritePayload {
            text: Some("hi".into()),
            html: Some(formats::cf_html_from_fragment("<b>hi</b>")),
            rtf: Some(r"{\rtf1 hi}".into()),
            image: Some(tuck_core::Image::new(1, 1)),
            files: vec![PathBuf::from(r"C:\a.txt")],
        };
        let entries = registered.entries(&payload, 0x0102_0304_0506_0708).unwrap();
        let ids: Vec<u32> = entries.iter().map(|(format, _)| *format).collect();
        assert_eq!(ids, [UNICODE_TEXT, 0xC002, 0xC003, 0xC001, DIBV5, HDROP, 0xC008, 0xC004]);
        assert_eq!(entries[0].1, formats::encode_utf16_text("hi"));
        assert_eq!(entries[6].1, 1u32.to_le_bytes());
        assert_eq!(entries[7].1, [8, 7, 6, 5, 4, 3, 2, 1]);
        assert!(registered.entries(&WritePayload::default(), 1).is_err());
    }

    #[test]
    fn plain_write_is_text_and_token_only() {
        let payload = WritePayload { text: Some("plain".into()), ..WritePayload::default() };
        let ids: Vec<u32> = fake_formats().entries(&payload, 7).unwrap().iter().map(|(format, _)| *format).collect();
        assert_eq!(ids, [UNICODE_TEXT, 0xC004]);
    }

    #[test]
    fn shell_hosts_are_recognized_by_file_name() {
        assert!(is_shell_host(Path::new(r"C:\Windows\Explorer.EXE")));
        assert!(is_shell_host(Path::new(r"C:\Windows\SystemApps\ShellExperienceHost.exe")));
        assert!(!is_shell_host(Path::new(r"C:\Program Files\App\app.exe")));
    }

    #[test]
    fn size_limit_is_inclusive() {
        assert_eq!(fits(10, 10), Ok(()));
        assert_eq!(fits(11, 10), Err(SkipReason::TooLarge));
    }
}
