//! The Tuck panel: the popup with Clipboard, Emoji, Kaomoji and Symbols tabs. See DESIGN.md §7 and `API.md`.

mod clipboard;
mod format;
mod panel;
mod picker;
mod style;

use std::rc::Rc;

use tuck_core::{ClipId, ClipItem, PointI, SkinTone, TextKind};
use tuck_ui::{Bitmap, Icon};

pub use format::Clock;
pub use panel::Panel;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Tab {
    #[default]
    Clipboard,
    Emoji,
    Kaomoji,
    Symbols,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Clipboard, Tab::Emoji, Tab::Kaomoji, Tab::Symbols];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(index: usize) -> Tab {
        Self::ALL[index % Self::ALL.len()]
    }

    pub fn title(self) -> &'static str {
        match self {
            Tab::Clipboard => "Clipboard",
            Tab::Emoji => "Emoji",
            Tab::Kaomoji => "Kaomoji",
            Tab::Symbols => "Symbols",
        }
    }

    pub fn placeholder(self) -> &'static str {
        match self {
            Tab::Clipboard => "Search clipboard",
            Tab::Emoji => "Search emoji",
            Tab::Kaomoji => "Search kaomoji",
            Tab::Symbols => "Search symbols",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Tab::Clipboard => Icon::Clipboard,
            Tab::Emoji => Icon::Smile,
            Tab::Kaomoji => Icon::Parentheses,
            Tab::Symbols => Icon::Omega,
        }
    }

    /// The next (or previous) tab, wrapping around.
    pub fn step(self, forward: bool) -> Tab {
        let n = Self::ALL.len();
        Tab::from_index(if forward { self.index() + 1 } else { self.index() + n - 1 })
    }
}

/// One emoji, kaomoji or symbol.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PickerItem {
    pub text: String,
    pub name: String,
    /// The six skin-tone variants in `SkinTone::ALL` order (the first is the default), or empty.
    pub variants: Vec<String>,
}

impl PickerItem {
    pub fn new(text: &str, name: &str) -> Self {
        Self { text: text.to_string(), name: name.to_string(), variants: Vec::new() }
    }

    pub fn with_variants(mut self, variants: Vec<String>) -> Self {
        self.variants = variants;
        self
    }

    /// The text to show and insert for `tone`.
    pub fn toned(&self, tone: SkinTone) -> &str {
        let index = SkinTone::ALL.iter().position(|t| *t == tone).unwrap_or(0);
        self.variants.get(index).map_or(self.text.as_str(), String::as_str)
    }

    pub fn has_tones(&self) -> bool {
        self.variants.len() == SkinTone::ALL.len()
    }
}

/// A titled group of picker items. An empty title draws no header. In the Emoji tab a first section with
/// `Icon::Clock` is "Frequently used" and shows at most two rows.
#[derive(Clone, Debug, PartialEq)]
pub struct PickerSection {
    pub title: String,
    pub icon: Icon,
    pub items: Vec<PickerItem>,
}

/// A clipboard item with what the card needs to draw it.
#[derive(Clone)]
pub struct ClipRow {
    pub item: ClipItem,
    pub kind: TextKind,
    /// About 2× the card size; None until `set_thumbnail` (the panel asks with `NeedThumbnails`).
    pub thumbnail: Option<Rc<Bitmap>>,
    pub app_icon: Option<Rc<Bitmap>>,
    pub app_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PanelAction {
    Insert { tab: Tab, text: String, close: bool },
    Paste { id: ClipId, plain: bool },
    Copy { id: ClipId },
    SetPinned { id: ClipId, pinned: bool },
    Delete { id: ClipId },
    ClearUnpinned,
    CopyImageText { id: ClipId },
    EditInGlint { id: ClipId },
    ShowInExplorer { id: ClipId },
    QueryChanged { tab: Tab, query: String },
    TabChanged(Tab),
    NeedThumbnails(Vec<ClipId>),
    SkinTone(SkinTone),
    SetPaused(bool),
    OpenSettings,
    /// New window origin, physical px.
    DragTo(PointI),
    Close,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PanelOptions {
    pub skin_tone: SkinTone,
    pub close_after_click: bool,
    pub plain_by_default: bool,
    pub glint_available: bool,
    pub paused: bool,
}
