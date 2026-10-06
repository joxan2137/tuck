use crate::{target::Target, util::hwnd};
use anyhow::{Context, Result};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tuck_core::{PointI, RectI};
use windows::{
    Win32::{
        Foundation::{HWND, RECT},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, SAFEARRAY,
            },
            Ole::{
                SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayUnaccessData,
            },
            Variant::VARIANT,
        },
        UI::{
            Accessibility::{
                AccessibleObjectFromWindow, CUIAutomation8, IAccessible, IUIAutomation, IUIAutomation2,
                IUIAutomationElement, IUIAutomationTextPattern, IUIAutomationTextPattern2, IUIAutomationTextRange,
                TextPatternRangeEndpoint, TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start,
                TextUnit_Character, UIA_TextPattern2Id, UIA_TextPatternId,
            },
            WindowsAndMessaging::{CHILDID_SELF, GetClassNameW, OBJID_CARET},
        },
    },
    core::{BOOL, Interface},
};

const SKIP_AFTER_MISS: Duration = Duration::from_secs(60);
const UIA_TIMEOUT_MS: u32 = 1000;
const ELEMENT_MAX_SHARE_OF_WORK_AREA: f64 = 0.8;
const CARET_GAP_DIP: f32 = 6.0;
const CARET_LEFT_INSET_DIP: f32 = 24.0;
const CURSOR_OFFSET_DIP: f32 = 12.0;
const EDGE_MARGIN_DIP: f32 = 8.0;

/// Where the panel anchors, in physical screen pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Caret(RectI),
    Element(RectI),
}

struct Request {
    focus: isize,
    chromium: bool,
    work_area: RectI,
    reply: mpsc::SyncSender<Option<Placement>>,
}

/// Finds the text caret of a `Target` when GetGUIThreadInfo has none, on an MTA worker with a warm UI Automation.
pub struct CaretLocator {
    requests: Option<mpsc::Sender<Request>>,
    busy: Arc<AtomicBool>,
    missed: Mutex<HashMap<u32, Instant>>,
    worker: Option<JoinHandle<()>>,
}
impl CaretLocator {
    pub fn start() -> Result<Self> {
        let (requests, receiver) = mpsc::channel();
        let (ready, started) = mpsc::sync_channel(1);
        let busy = Arc::new(AtomicBool::new(false));
        let worker_busy = busy.clone();
        let worker =
            thread::Builder::new().name("tuck-caret".into()).spawn(move || run_worker(receiver, worker_busy, ready))?;
        match started.recv().context("Caret worker exited during startup")? {
            Ok(()) => {
                Ok(Self { requests: Some(requests), busy, missed: Mutex::new(HashMap::new()), worker: Some(worker) })
            }
            Err(error) => {
                let _ = worker.join();
                Err(error)
            }
        }
    }

    /// `target.caret` if known, else the worker's answer if it arrives within `budget`. A process whose answer came
    /// too late is not asked again for 60 s.
    pub fn locate(&self, target: &Target, budget: Duration) -> Option<Placement> {
        if let Some(caret) = target.caret {
            return Some(Placement::Caret(caret));
        }
        if self.recently_missed(target.pid) || self.busy.swap(true, Ordering::AcqRel) {
            return None;
        }
        let (reply, answer) = mpsc::sync_channel(1);
        let request = Request {
            focus: target.focus,
            chromium: is_chromium_window(hwnd(target.focus)) || is_chromium_window(hwnd(target.hwnd)),
            work_area: target.work_area,
            reply,
        };
        if self.requests.as_ref().is_none_or(|requests| requests.send(request).is_err()) {
            self.busy.store(false, Ordering::Release);
            return None;
        }
        match answer.recv_timeout(budget) {
            Ok(placement) => placement,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.lock_missed().insert(target.pid, Instant::now());
                None
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
        }
    }

    fn lock_missed(&self) -> std::sync::MutexGuard<'_, HashMap<u32, Instant>> {
        self.missed.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn recently_missed(&self, pid: u32) -> bool {
        let mut missed = self.lock_missed();
        missed.retain(|_, at| at.elapsed() < SKIP_AFTER_MISS);
        missed.contains_key(&pid)
    }
}
impl Drop for CaretLocator {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(worker) = self.worker.take()
            && !self.busy.load(Ordering::Acquire)
        {
            let _ = worker.join();
        }
    }
}

fn run_worker(receiver: mpsc::Receiver<Request>, busy: Arc<AtomicBool>, ready: mpsc::SyncSender<Result<()>>) {
    if let Err(error) = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok() {
        let _ = ready.send(Err(error.into()));
        return;
    }
    match create_automation() {
        Ok(automation) => {
            let _ = ready.send(Ok(()));
            while let Ok(request) = receiver.recv() {
                let placement = locate_on_worker(&automation, &request);
                let _ = request.reply.send(placement);
                busy.store(false, Ordering::Release);
            }
        }
        Err(error) => {
            let _ = ready.send(Err(error));
        }
    }
    unsafe { CoUninitialize() };
}

fn create_automation() -> Result<IUIAutomation> {
    let automation: IUIAutomation = unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)? };
    if let Ok(automation) = automation.cast::<IUIAutomation2>() {
        unsafe {
            let _ = automation.SetConnectionTimeout(UIA_TIMEOUT_MS);
            let _ = automation.SetTransactionTimeout(UIA_TIMEOUT_MS);
        }
    }
    let _ = unsafe { automation.GetRootElement() };
    Ok(automation)
}

fn locate_on_worker(automation: &IUIAutomation, request: &Request) -> Option<Placement> {
    if let Some(caret) = msaa_caret(hwnd(request.focus)) {
        return Some(Placement::Caret(caret));
    }
    if request.chromium {
        return None;
    }
    let element = unsafe { automation.GetFocusedElement() }.ok()?;
    if let Some(caret) = text_caret(&element) {
        return Some(Placement::Caret(caret));
    }
    let bounds = rect_i(unsafe { element.CurrentBoundingRectangle() }.ok()?);
    fits_as_element(bounds, request.work_area).then_some(Placement::Element(bounds))
}

fn rect_i(rect: RECT) -> RectI {
    RectI::from_ltrb(rect.left, rect.top, rect.right, rect.bottom)
}

/// Chromium turns its whole accessibility tree on when asked for UIA or OBJID_CLIENT; only OBJID_CARET is safe.
fn is_chromium_class(class: &str) -> bool {
    class.starts_with("Chrome_WidgetWin_") || class == "Chrome_RenderWidgetHostHWND"
}

fn is_chromium_window(window: HWND) -> bool {
    let mut buffer = [0u16; 64];
    let length = unsafe { GetClassNameW(window, &mut buffer) };
    length > 0 && is_chromium_class(&String::from_utf16_lossy(&buffer[..length as usize]))
}

fn plausible_caret(rect: RectI) -> Option<RectI> {
    (rect.h > 0 && rect.w >= 0 && (rect.x, rect.y) != (0, 0)).then_some(rect)
}

fn msaa_caret(focus: HWND) -> Option<RectI> {
    let mut object = std::ptr::null_mut();
    unsafe { AccessibleObjectFromWindow(focus, OBJID_CARET.0 as u32, &IAccessible::IID, &mut object) }.ok()?;
    if object.is_null() {
        return None;
    }
    let accessible = unsafe { IAccessible::from_raw(object) };
    let (mut left, mut top, mut width, mut height) = (0, 0, 0, 0);
    let child = VARIANT::from(CHILDID_SELF as i32);
    unsafe { accessible.accLocation(&mut left, &mut top, &mut width, &mut height, &child) }.ok()?;
    plausible_caret(RectI::new(left, top, width, height))
}

fn caret_range(element: &IUIAutomationElement) -> Option<IUIAutomationTextRange> {
    if let Ok(pattern) = unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPattern2Id) } {
        let mut active = BOOL::default();
        if let Ok(range) = unsafe { pattern.GetCaretRange(&mut active) } {
            return Some(range);
        }
    }
    let pattern = unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }.ok()?;
    let selection = unsafe { pattern.GetSelection() }.ok()?;
    if unsafe { selection.Length() }.ok()? < 1 {
        return None;
    }
    unsafe { selection.GetElement(0) }.ok()
}

/// A degenerate caret range often has no rectangle; then measure the character after it (left edge) or before it
/// (right edge).
fn text_caret(element: &IUIAutomationElement) -> Option<RectI> {
    let range = caret_range(element)?;
    first_rect(&range)
        .or_else(|| neighbour_edge(&range, TextPatternRangeEndpoint_End, 1))
        .or_else(|| neighbour_edge(&range, TextPatternRangeEndpoint_Start, -1))
        .and_then(plausible_caret)
}

fn neighbour_edge(
    range: &IUIAutomationTextRange,
    endpoint: TextPatternRangeEndpoint,
    characters: i32,
) -> Option<RectI> {
    let probe = unsafe { range.Clone() }.ok()?;
    if unsafe { probe.MoveEndpointByUnit(endpoint, TextUnit_Character, characters) }.ok()? == 0 {
        return None;
    }
    let character = first_rect(&probe)?;
    let x = if characters < 0 { character.right() } else { character.x };
    Some(RectI::new(x, character.y, 1, character.h))
}

fn first_rect(range: &IUIAutomationTextRange) -> Option<RectI> {
    let array = unsafe { range.GetBoundingRectangles() }.ok()?;
    rects_from_doubles(&take_doubles(array)).into_iter().next()
}

/// Reads and destroys a one-dimensional SAFEARRAY of doubles.
fn take_doubles(array: *mut SAFEARRAY) -> Vec<f64> {
    if array.is_null() {
        return Vec::new();
    }
    let values = unsafe {
        match (SafeArrayGetLBound(array, 1), SafeArrayGetUBound(array, 1)) {
            (Ok(lower), Ok(upper)) if upper >= lower => {
                let mut data = std::ptr::null_mut();
                if SafeArrayAccessData(array, &mut data).is_ok() && !data.is_null() {
                    let count = (upper - lower + 1) as usize;
                    let values = std::slice::from_raw_parts(data.cast::<f64>(), count).to_vec();
                    let _ = SafeArrayUnaccessData(array);
                    values
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    };
    unsafe {
        let _ = SafeArrayDestroy(array);
    }
    values
}

fn rects_from_doubles(values: &[f64]) -> Vec<RectI> {
    values
        .chunks_exact(4)
        .map(|r| RectI::new(r[0].round() as i32, r[1].round() as i32, r[2].round() as i32, r[3].round() as i32))
        .filter(|rect| rect.h > 0)
        .collect()
}

fn fits_as_element(bounds: RectI, work_area: RectI) -> bool {
    let area = |rect: RectI| rect.w as f64 * rect.h as f64;
    !bounds.is_empty() && area(bounds) < ELEMENT_MAX_SHARE_OF_WORK_AREA * area(work_area)
}

fn clamp_axis(start: i32, size: i32, area_start: i32, area_end: i32, margin: i32) -> i32 {
    start.min(area_end - margin - size).max(area_start + margin)
}

/// Panel origin (physical px) for DESIGN §8: below the caret with a 6 DIP gap (above when there is no room), left
/// edge 24 DIP left of the caret; `Element` anchors to the element's bottom-left; no placement puts it 12 DIP
/// below-right of the cursor (flipped left/up when it would not fit). Clamped to the work area with an 8 DIP margin.
pub fn panel_origin(
    placement: Option<Placement>,
    cursor: PointI,
    panel_px: (i32, i32),
    work_area: RectI,
    scale: f32,
) -> PointI {
    let px = |dip: f32| (dip * scale).round() as i32;
    let (width, height) = panel_px;
    let margin = px(EDGE_MARGIN_DIP);
    let right_limit = work_area.right() - margin;
    let bottom_limit = work_area.bottom() - margin;
    let (x, below, above) = match placement {
        Some(Placement::Caret(caret)) => (
            caret.x - px(CARET_LEFT_INSET_DIP),
            caret.bottom() + px(CARET_GAP_DIP),
            caret.y - px(CARET_GAP_DIP) - height,
        ),
        Some(Placement::Element(element)) => {
            (element.x, element.bottom() + px(CARET_GAP_DIP), element.y - px(CARET_GAP_DIP) - height)
        }
        None => {
            let offset = px(CURSOR_OFFSET_DIP);
            let right = cursor.x + offset;
            let left = cursor.x - offset - width;
            let x = if right + width > right_limit && left >= work_area.x + margin { left } else { right };
            (x, cursor.y + offset, cursor.y - offset - height)
        }
    };
    let y = if below + height > bottom_limit && above >= work_area.y + margin { above } else { below };
    PointI::new(
        clamp_axis(x, width, work_area.x, work_area.right(), margin),
        clamp_axis(y, height, work_area.y, work_area.bottom(), margin),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: RectI = RectI::new(0, 0, 1920, 1040);
    const PANEL: (i32, i32) = (380, 460);

    fn origin(placement: Option<Placement>, cursor: PointI, scale: f32) -> PointI {
        panel_origin(placement, cursor, PANEL, WORK, scale)
    }

    #[test]
    fn below_the_caret_with_left_inset() {
        let caret = Placement::Caret(RectI::new(500, 300, 2, 20));
        assert_eq!(origin(Some(caret), PointI::default(), 1.0), PointI::new(476, 326));
        assert_eq!(origin(Some(caret), PointI::default(), 1.5), PointI::new(464, 329));
        assert_eq!(origin(Some(caret), PointI::default(), 2.0), PointI::new(452, 332));
    }

    #[test]
    fn above_the_caret_when_there_is_no_room_below() {
        let caret = Placement::Caret(RectI::new(500, 900, 2, 20));
        assert_eq!(origin(Some(caret), PointI::default(), 1.0), PointI::new(476, 434));
        let fits_exactly = Placement::Caret(RectI::new(500, 540, 2, 20));
        assert_eq!(origin(Some(fits_exactly), PointI::default(), 1.0).y, 566);
        let tight = Placement::Caret(RectI::new(500, 300, 2, 700));
        assert_eq!(origin(Some(tight), PointI::default(), 1.0).y, 1040 - 8 - 460);
    }

    #[test]
    fn clamped_to_the_work_area_margin() {
        let right = Placement::Caret(RectI::new(1900, 300, 2, 20));
        assert_eq!(origin(Some(right), PointI::default(), 1.0).x, 1920 - 8 - 380);
        let left = Placement::Caret(RectI::new(10, 300, 2, 20));
        assert_eq!(origin(Some(left), PointI::default(), 1.0).x, 8);
        assert_eq!(origin(Some(left), PointI::default(), 2.0).x, 16);
        let tiny = RectI::new(100, 50, 300, 200);
        assert_eq!(panel_origin(Some(left), PointI::default(), PANEL, tiny, 1.0), PointI::new(108, 58));
    }

    #[test]
    fn element_anchors_bottom_left() {
        let element = Placement::Element(RectI::new(200, 200, 600, 30));
        assert_eq!(origin(Some(element), PointI::default(), 1.0), PointI::new(200, 236));
        let low = Placement::Element(RectI::new(200, 800, 600, 30));
        assert_eq!(origin(Some(low), PointI::default(), 1.0), PointI::new(200, 334));
    }

    #[test]
    fn cursor_placement_goes_below_right_and_flips_near_edges() {
        assert_eq!(origin(None, PointI::new(100, 100), 1.0), PointI::new(112, 112));
        assert_eq!(origin(None, PointI::new(100, 100), 2.0), PointI::new(124, 124));
        assert_eq!(origin(None, PointI::new(1900, 1000), 1.0), PointI::new(1508, 528));
    }

    #[test]
    fn secondary_monitor_coordinates() {
        let work = RectI::new(-1280, -200, 1280, 1000);
        let caret = Placement::Caret(RectI::new(-1270, -190, 2, 18));
        assert_eq!(panel_origin(Some(caret), PointI::default(), PANEL, work, 1.25), PointI::new(-1270, -164));
    }

    #[test]
    fn chromium_classes() {
        assert!(is_chromium_class("Chrome_WidgetWin_1"));
        assert!(is_chromium_class("Chrome_WidgetWin_0"));
        assert!(is_chromium_class("Chrome_RenderWidgetHostHWND"));
        assert!(!is_chromium_class("Notepad"));
        assert!(!is_chromium_class("MozillaWindowClass"));
    }

    #[test]
    fn rectangles_and_element_size_rules() {
        assert_eq!(rects_from_doubles(&[10.4, 20.6, 1.0, 18.0, 0.0, 0.0, 0.0, 0.0, 5.0]), [RectI::new(10, 21, 1, 18)]);
        assert!(fits_as_element(RectI::new(0, 0, 800, 30), WORK));
        assert!(!fits_as_element(RectI::new(0, 0, 1900, 1000), WORK));
        assert!(!fits_as_element(RectI::default(), WORK));
        assert_eq!(plausible_caret(RectI::new(0, 0, 0, 0)), None);
        assert_eq!(plausible_caret(RectI::new(300, 200, 0, 17)), Some(RectI::new(300, 200, 0, 17)));
    }
}
