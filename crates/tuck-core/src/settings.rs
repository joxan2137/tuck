use serde::{Deserialize, Serialize};

/// Saved as JSON in `%APPDATA%\Tuck\settings.json`. Every field has a default so older files keep loading.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeMode,
    pub launch_at_login: bool,
    pub shortcuts: ShortcutSettings,
    pub history: HistorySettings,
    pub paste: PasteSettings,
    pub emoji: EmojiSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::default(),
            launch_at_login: true,
            shortcuts: ShortcutSettings::default(),
            history: HistorySettings::default(),
            paste: PasteSettings::default(),
            emoji: EmojiSettings::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShortcutSettings {
    /// Win+V opens the panel on the Clipboard tab.
    pub win_v: bool,
    /// Win+. and Win+; open the panel on the Emoji tab.
    pub win_period: bool,
}

impl Default for ShortcutSettings {
    fn default() -> Self {
        Self { win_v: true, win_period: true }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    /// Record new clipboard content. Off = paused.
    pub enabled: bool,
    /// Unpinned items kept; pinned items never count.
    pub max_items: usize,
    /// Keep unpinned items across restarts (pinned items are always kept).
    pub keep_after_restart: bool,
    /// Skip a single copy larger than this.
    pub max_item_mb: u32,
    /// Executable file names (lowercase, e.g. `keepass.exe`) whose copies are never recorded.
    pub ignored_apps: Vec<String>,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self { enabled: true, max_items: 200, keep_after_restart: true, max_item_mb: 32, ignored_apps: Vec::new() }
    }
}

pub const MAX_ITEMS_CHOICES: [usize; 6] = [25, 50, 100, 200, 500, 1000];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PasteKey {
    #[default]
    CtrlV,
    ShiftInsert,
    CtrlShiftV,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasteOverride {
    /// Executable file name, lowercase.
    pub exe: String,
    pub key: PasteKey,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PasteSettings {
    /// Enter pastes plain text; Shift+Enter keeps formatting.
    pub plain_text_by_default: bool,
    pub overrides: Vec<PasteOverride>,
}

impl Default for PasteSettings {
    fn default() -> Self {
        let shift_insert = |exe: &str| PasteOverride { exe: exe.into(), key: PasteKey::ShiftInsert };
        Self { plain_text_by_default: false, overrides: vec![shift_insert("putty.exe"), shift_insert("mintty.exe")] }
    }
}

impl PasteSettings {
    pub fn key_for(&self, exe_name: Option<&str>) -> PasteKey {
        let Some(exe) = exe_name else { return PasteKey::CtrlV };
        self.overrides.iter().find(|o| o.exe.eq_ignore_ascii_case(exe)).map_or(PasteKey::CtrlV, |o| o.key)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SkinTone {
    #[default]
    Default,
    Light,
    MediumLight,
    Medium,
    MediumDark,
    Dark,
}

impl SkinTone {
    pub const ALL: [SkinTone; 6] =
        [Self::Default, Self::Light, Self::MediumLight, Self::Medium, Self::MediumDark, Self::Dark];

    /// The Fitzpatrick modifier code point, `None` for the default (yellow) tone.
    pub fn modifier(self) -> Option<char> {
        match self {
            Self::Default => None,
            Self::Light => Some('\u{1F3FB}'),
            Self::MediumLight => Some('\u{1F3FC}'),
            Self::Medium => Some('\u{1F3FD}'),
            Self::MediumDark => Some('\u{1F3FE}'),
            Self::Dark => Some('\u{1F3FF}'),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EmojiSettings {
    pub skin_tone: SkinTone,
    /// A mouse click inserts and closes the panel. Off = the panel stays open for more picks (Enter always closes).
    pub close_after_click: bool,
}
