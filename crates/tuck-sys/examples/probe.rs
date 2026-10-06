//! Read-only probe: prints the foreground target, its caret placement (both paths, timed) and app name.
//! No hook, no window, no input. `cargo run -p tuck-sys --example probe -- [delay seconds]`

use std::time::{Duration, Instant};
use tuck_sys::{CaretLocator, Target, apps, capture_target};
use windows::Win32::{
    System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx},
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
};

const WORKER_BUDGET: Duration = Duration::from_millis(500);

fn millis(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn print_target(target: &Target, took_ms: f64) {
    println!("capture_target: {took_ms:.3} ms");
    println!("  hwnd 0x{:x}  focus 0x{:x}  thread {}  pid {}", target.hwnd, target.focus, target.thread_id, target.pid);
    println!("  exe       {}", target.exe.as_ref().map_or("<unknown>".into(), |exe| exe.display().to_string()));
    println!("  layout    0x{:08x}", target.layout as usize & 0xffff_ffff);
    println!("  caret     {:?}", target.caret);
    println!("  work area {:?}", target.work_area);
    println!("  elevated  {}", target.elevated);
}

fn main() -> anyhow::Result<()> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)?;
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    if let Some(delay) = std::env::args().nth(1).and_then(|arg| arg.parse::<u64>().ok()) {
        println!("Waiting {delay} s; focus the window to probe.");
        std::thread::sleep(Duration::from_secs(delay));
    }
    let start = Instant::now();
    let Some(target) = capture_target() else {
        println!("No foreground window.");
        return Ok(());
    };
    print_target(&target, millis(start));

    let start = Instant::now();
    let locator = CaretLocator::start()?;
    println!("CaretLocator::start: {:.1} ms", millis(start));

    let start = Instant::now();
    let placement = locator.locate(&target, Duration::from_millis(20));
    println!("locate (sync path first, 20 ms budget): {placement:?} in {:.3} ms", millis(start));

    let worker_target = Target { caret: None, ..target.clone() };
    for attempt in 1..=2 {
        let start = Instant::now();
        let placement = locator.locate(&worker_target, WORKER_BUDGET);
        println!(
            "locate (worker path #{attempt}, {} ms budget): {placement:?} in {:.3} ms",
            WORKER_BUDGET.as_millis(),
            millis(start)
        );
    }

    if let Some(exe) = &target.exe {
        let start = Instant::now();
        let name = apps::display_name(exe);
        let first = millis(start);
        let start = Instant::now();
        apps::display_name(exe);
        println!("display_name: {name:?} ({first:.3} ms, cached {:.3} ms)", millis(start));
        let start = Instant::now();
        let icon = apps::icon(exe, 32);
        println!(
            "icon(32): {} in {:.3} ms",
            icon.map_or("none".into(), |image| format!("{}x{}", image.width, image.height)),
            millis(start)
        );
    }
    Ok(())
}
