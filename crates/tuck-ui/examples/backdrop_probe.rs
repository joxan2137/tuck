//! Decides which `PanelBackdrop` ships (DESIGN §9). Shows real windows: only the main thread runs this.
//!
//! cargo run -p tuck-ui --example backdrop_probe -- --monitor-x <px> --monitor-y <px> --seconds N --out <dir>
//!
//! Opens a busy test-pattern window at (monitor-x, monitor-y) and, for each mode, a panel window on top of it without
//! activation. About 600 ms after the panel appears its screen rect is copied with BitBlt to `<dir>/<mode>.png`
//! (`<mode>-context.png` adds a 24 px margin to show corners and edges). Each panel stays up for `--seconds`
//! (default 1, at least the capture time) before the next mode opens. Nothing is activated and nothing else is
//! touched.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result};
use tuck_ui::{
    App, Brush, Color, Ctx, Image, Painter, PanelBackdrop, PointF, PointI, RectF, RectI, SizeF, StrokeStyle, TextStyle,
    View, Weight, WindowSpec, run,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SRCCOPY, SelectObject,
};

const PANEL: SizeF = SizeF::new(380.0, 460.0);
const PATTERN: SizeF = SizeF::new(720.0, 640.0);
const CAPTURE_DELAY: Duration = Duration::from_millis(600);
const CONTEXT_MARGIN: i32 = 24;

struct Options {
    origin: PointI,
    hold: Duration,
    out: PathBuf,
}

fn options() -> Options {
    let args: Vec<String> = std::env::args().collect();
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let number = |name: &str| value(name).and_then(|v| v.parse::<f64>().ok());
    Options {
        origin: PointI::new(number("--monitor-x").unwrap_or(0.0) as i32, number("--monitor-y").unwrap_or(0.0) as i32),
        hold: Duration::from_secs_f64(number("--seconds").unwrap_or(1.0)).max(CAPTURE_DELAY + Duration::from_millis(100)),
        out: value("--out").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("tuck-backdrop-probe")),
    }
}

fn main() -> Result<()> {
    tuck_ui::enable_per_monitor_dpi_awareness();
    let options = Rc::new(options());
    std::fs::create_dir_all(&options.out).with_context(|| format!("creating {}", options.out.display()))?;
    run(move |app: &App| {
        let pattern = WindowSpec::popup(options.origin, PATTERN).topmost(true);
        app.open(pattern, PatternView)?;
        let options = options.clone();
        app.set_timer(Duration::from_millis(400), move |app| probe(app, options, 0));
        Ok(())
    })
}

/// Shows the panel for mode `index`, captures it, closes it and moves on to the next mode.
fn probe(app: &App, options: Rc<Options>, index: usize) {
    let Some(&backdrop) = PanelBackdrop::ALL.get(index) else {
        println!("done: {}", options.out.display());
        app.quit();
        return;
    };
    let origin = PointI::new(options.origin.x + 120, options.origin.y + 90);
    let spec = WindowSpec::panel(origin, PANEL).backdrop(backdrop);
    let id = match app.open(spec, ProbePanel { backdrop }) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("{}: could not open the panel: {e:#}", backdrop.name());
            probe(app, options, index + 1);
            return;
        }
    };
    let shown = Rc::new(Cell::new(false));
    let capture_options = options.clone();
    let captured = shown.clone();
    app.set_timer(CAPTURE_DELAY, move |app| {
        let Some(rect) = app.hwnd(id).and_then(tuck_ui::win::window_rect_px) else { return };
        for (suffix, margin) in [("", 0), ("-context", CONTEXT_MARGIN)] {
            let area = RectI::new(rect.x - margin, rect.y - margin, rect.w + 2 * margin, rect.h + 2 * margin);
            let path = capture_options.out.join(format!("{}{suffix}.png", backdrop.name()));
            match capture_screen(area).and_then(|image| save_png(&path, &image)) {
                Ok(()) => println!("{}: {} ({}x{} px at {},{})", backdrop.name(), path.display(), area.w, area.h, area.x, area.y),
                Err(e) => eprintln!("{}: capture failed: {e:#}", backdrop.name()),
            }
        }
        captured.set(true);
    });
    app.set_timer(options.hold, move |app| {
        if !shown.get() {
            eprintln!("{}: closed before the capture ran", backdrop.name());
        }
        app.close(id);
        let options = options.clone();
        app.set_timer(Duration::from_millis(250), move |app| probe(app, options, index + 1));
    });
}

/// Copies a physical-pixel screen rect (everything DWM composed there) into an opaque image.
fn capture_screen(rect: RectI) -> Result<Image> {
    anyhow::ensure!(rect.w > 0 && rect.h > 0, "empty capture rect");
    let mut image = Image::new(rect.w as u32, rect.h as u32);
    // SAFETY: GDI objects are created, selected, used and released in balanced pairs within this block; the DIB
    // buffer holds exactly width * height 32-bit pixels.
    unsafe {
        let screen = GetDC(None);
        let memory = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, rect.w, rect.h);
        let previous = SelectObject(memory, bitmap.into());
        let copied = BitBlt(memory, 0, 0, rect.w, rect.h, Some(screen), rect.x, rect.y, SRCCOPY | CAPTUREBLT);
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: rect.w,
                biHeight: -rect.h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        SelectObject(memory, previous);
        let lines = GetDIBits(memory, bitmap, 0, rect.h as u32, Some(image.data.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        copied.context("BitBlt from the screen")?;
        anyhow::ensure!(lines == rect.h, "GetDIBits copied {lines} of {} rows", rect.h);
    }
    for pixel in image.data.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    Ok(image)
}

fn save_png(path: &Path, image: &Image) -> Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    let rgba: Vec<u8> = image.data.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
    writer.write_image_data(&rgba)?;
    Ok(())
}

/// Saturated blobs, fine stripes and text: blur strength, tint and legibility are easy to judge over it.
struct PatternView;

impl View for PatternView {
    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter) {
        let size = cx.size();
        let bounds = RectF::new(0.0, 0.0, size.w, size.h);
        let stops = [
            (0.0, Color::hex("#1B3A6B").unwrap()),
            (0.4, Color::hex("#6A2C91").unwrap()),
            (0.75, Color::hex("#C2185B").unwrap()),
            (1.0, Color::hex("#F57C00").unwrap()),
        ];
        p.fill_rect(bounds, Brush::linear(PointF::new(0.0, 0.0), PointF::new(size.w, size.h), &stops));
        for (hex, x, y, r) in [("#00BCD4", 0.2, 0.2, 90.0), ("#FFEB3B", 0.75, 0.15, 70.0), ("#4CAF50", 0.35, 0.7, 100.0), ("#FFFFFF", 0.7, 0.75, 60.0)] {
            p.fill_circle(PointF::new(size.w * x, size.h * y), r, Color::hex(hex).unwrap());
        }
        let mut x = -size.h;
        while x < size.w {
            p.line(PointF::new(x, size.h), PointF::new(x + size.h, 0.0), Color::rgba(0.0, 0.0, 0.0, 0.35), 3.0, &StrokeStyle::default());
            x += 16.0;
        }
        let words = TextStyle::new(24.0).weight(Weight::Bold);
        let mut y = 20.0;
        while y < size.h {
            p.text("Tuck backdrop probe · The quick brown fox", &words, Color::WHITE, RectF::new(16.0, y, size.w, 32.0));
            y += 56.0;
        }
    }
}

/// What the panel paints on top of the DWM material: DESIGN §9's glass fill at reduced alpha (opaque for Solid),
/// the hairline, and a few lines of text to judge legibility.
struct ProbePanel {
    backdrop: PanelBackdrop,
}

impl View for ProbePanel {
    fn paint(&mut self, cx: &mut Ctx, p: &mut Painter) {
        let theme = p.theme().clone();
        let bounds = p.bounds();
        let radius = 8.0;
        let fill = match (self.backdrop.is_opaque(), theme.is_dark()) {
            (true, true) => Color::rgb8(30, 30, 32),
            (true, false) => Color::rgb8(246, 246, 248),
            (false, true) => Color::rgba8(30, 30, 32, 0.55),
            (false, false) => Color::rgba8(246, 246, 248, 0.62),
        };
        p.fill_round_rect(bounds, radius, fill);
        p.hairline_round_rect(bounds, radius, theme.hairline, true);
        p.text(self.backdrop.name(), &TextStyle::title(), theme.text, RectF::new(16.0, 16.0, bounds.w - 32.0, 24.0));
        let lines = ["Clipboard history", "https://example.com/a/long/path", "Secondary text", "Tertiary text"];
        let colors = [theme.text, theme.accent, theme.text_secondary, theme.text_tertiary];
        for (i, (line, color)) in lines.iter().zip(colors).enumerate() {
            let row = RectF::new(12.0, 60.0 + i as f32 * 48.0, bounds.w - 24.0, 40.0);
            p.fill_round_rect(row, 10.0, theme.hover);
            p.text(line, &TextStyle::body(), color, row.inset(12.0));
        }
        p.text(&format!("scale {:.2}", cx.scale()), &TextStyle::caption(), theme.text_secondary, RectF::new(16.0, bounds.h - 32.0, 200.0, 20.0));
    }
}
