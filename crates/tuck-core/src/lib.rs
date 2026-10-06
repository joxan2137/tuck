//! Tuck's shared, Windows-free pieces: geometry, images, settings, clipboard items and history.

pub mod clip;
pub mod geom;
pub mod history;
pub mod image;
pub mod settings;
pub mod thumb;

pub use clip::{ClipContent, ClipId, ClipItem, ClipKind, Rgba, SourceApp, TextKind, classify};
pub use geom::{PointF, PointI, RectF, RectI, SizeF};
pub use history::{AddOutcome, History, Op, content_bytes};
pub use image::Image;
pub use settings::{PasteKey, Settings, SkinTone, ThemeMode};
pub use thumb::thumbnail;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
