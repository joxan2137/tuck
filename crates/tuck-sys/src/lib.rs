//! Windows plumbing for Tuck: input hook, key translation, text injection, caret lookup, target app info,
//! tray, single instance, install, logging, paths, settings file, OCR. See DESIGN.md §4 and `API.md`.

pub mod apps;
pub mod caret;
pub mod hook;
pub mod inject;
pub mod install;
pub mod instance;
pub mod keys;
pub mod logging;
pub mod ocr;
pub mod paths;
pub mod settings_store;
pub mod shell;
pub mod target;
pub mod tray;

mod util;

pub use caret::{CaretLocator, Placement, panel_origin};
pub use hook::{DismissReason, HookConfig, HookEvent, Hotkey, InputHook, MARKER, Mods, RoutedKey};
pub use inject::{paste, type_text};
pub use keys::{KeyText, TextTranslator};
pub use target::{ForegroundWatch, Target, capture_target};
